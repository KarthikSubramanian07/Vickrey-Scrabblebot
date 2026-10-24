//! Read the live server's public history and refit the field model.
//!
//! SpacetimeDB answers anonymous SQL over HTTP for public tables, so the
//! whole league's auction history (`auction_result`), winning bids of
//! finished matches (`auction_top_bid_archive`) and rosters
//! (`match_participant`, `bot`) are one POST away.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use vickey_brain::FieldModel;
use vickey_core::{index, ALPHABET};

pub const DEFAULT_HOST: &str = "https://maincloud.spacetimedb.com";
pub const DEFAULT_DB: &str = "scrabblebot";

/// Run one SQL query; rows come back as JSON arrays in column order.
pub fn sql(host: &str, db: &str, query: &str) -> Result<Vec<Vec<Value>>> {
    let url = format!("{host}/v1/database/{db}/sql");
    let body: Value = ureq::post(&url)
        .set("Content-Type", "text/plain")
        .timeout(std::time::Duration::from_secs(60))
        .send_string(query)
        .with_context(|| format!("POST {url}"))?
        .into_json()?;
    let rows = body
        .get(0)
        .and_then(|r| r.get("rows"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("unexpected SQL response"))?;
    Ok(rows.iter().filter_map(|r| r.as_array().cloned()).collect())
}

fn u64_of(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_i64().map(|x| x as u64))
}

/// `Option<T>` is encoded as a sum: `[0, value]` for some, `[1, []]` for none.
fn option_u64(v: &Value) -> Option<u64> {
    let a = v.as_array()?;
    if a.first()?.as_u64()? == 0 {
        u64_of(a.get(1)?)
    } else {
        None
    }
}

#[derive(Clone, Debug)]
pub struct LiveAuction {
    pub auction_id: u64,
    pub match_id: u64,
    pub letter: usize,
    pub winner: Option<u64>,
    pub paid: i64,
}

pub struct LiveHistory {
    pub auctions: Vec<LiveAuction>,
    pub top_bid: HashMap<u64, i64>,
    /// Ended matches whose seats were all real (non-sim) bots.
    pub all_real: HashSet<u64>,
}

pub fn fetch(host: &str, db: &str) -> Result<LiveHistory> {
    let sims: HashSet<u64> = sql(host, db, "SELECT id, is_simulated FROM bot")?
        .iter()
        .filter(|r| r.get(1).and_then(Value::as_bool) == Some(true))
        .filter_map(|r| r.first().and_then(u64_of))
        .collect();
    // status is a sum: tag 2 = Ended (Lobby, Running, Ended).
    let ended: HashSet<u64> = sql(host, db, "SELECT id, status FROM match_state")?
        .iter()
        .filter(|r| r.get(1).and_then(|s| s.get(0)).and_then(Value::as_u64) == Some(2))
        .filter_map(|r| r.first().and_then(u64_of))
        .collect();
    let mut has_sim: HashSet<u64> = HashSet::new();
    for r in sql(host, db, "SELECT match_id, bot_id FROM match_participant")? {
        let (Some(m), Some(b)) = (r.first().and_then(u64_of), r.get(1).and_then(u64_of)) else {
            continue;
        };
        if sims.contains(&b) {
            has_sim.insert(m);
        }
    }
    let all_real: HashSet<u64> = ended
        .iter()
        .copied()
        .filter(|m| !has_sim.contains(m))
        .collect();
    let auctions = sql(
        host,
        db,
        "SELECT auction_id, match_id, letter, winner_bot_id, paid FROM auction_result",
    )?
    .iter()
    .filter_map(|r| {
        Some(LiveAuction {
            auction_id: u64_of(r.first()?)?,
            match_id: u64_of(r.get(1)?)?,
            letter: index(r.get(2)?.as_str()?.chars().next()?)?,
            winner: option_u64(r.get(3)?),
            paid: r.get(4)?.as_i64()?,
        })
    })
    .collect();
    let top_bid = sql(
        host,
        db,
        "SELECT auction_id, top_bid FROM auction_top_bid_archive",
    )?
    .iter()
    .filter_map(|r| Some((u64_of(r.first()?)?, r.get(1)?.as_i64()?)))
    .collect();
    Ok(LiveHistory {
        auctions,
        top_bid,
        all_real,
    })
}

/// Fit the field model on the most recent `last_matches` all-real matches.
pub fn refit(h: &LiveHistory, last_matches: usize, prior: &FieldModel) -> (FieldModel, usize) {
    let mut recent: Vec<u64> = h.all_real.iter().copied().collect();
    recent.sort_unstable();
    let keep: HashSet<u64> = recent.iter().rev().take(last_matches).copied().collect();
    let mut by_match: HashMap<u64, Vec<&LiveAuction>> = HashMap::new();
    for a in &h.auctions {
        if keep.contains(&a.match_id) {
            by_match.entry(a.match_id).or_default().push(a);
        }
    }
    let mut logs: Vec<Vec<f64>> = vec![Vec::new(); ALPHABET];
    let mut seq: Vec<(usize, f64, usize)> = Vec::new(); // (letter, ln top, phase bin)
    let bins = prior.phase.len().max(2);
    for list in by_match.values_mut() {
        list.sort_by_key(|a| a.auction_id);
        let n = list.len();
        for (i, a) in list.iter().enumerate() {
            let Some(&t) = h.top_bid.get(&a.auction_id) else {
                continue;
            };
            if t <= 0 {
                continue;
            }
            let ln = (t as f64).ln();
            logs[a.letter].push(ln);
            seq.push((a.letter, ln, (i * bins / n.max(1)).min(bins - 1)));
        }
    }
    let median = |v: &mut Vec<f64>| -> Option<f64> {
        if v.len() < 8 {
            return None;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Some(v[v.len() / 2])
    };
    let mut model = prior.clone();
    let mut ln_med = [0.0; ALPHABET];
    let mut sigmas = Vec::new();
    for l in 0..ALPHABET {
        let mut v = logs[l].clone();
        match median(&mut v) {
            Some(m) => {
                ln_med[l] = m;
                // Same convention as the defaults: rivals' max sits ~10% under the table's top bid.
                model.median[l] = m.exp() * 0.9;
                let mean = v.iter().sum::<f64>() / v.len() as f64;
                sigmas.push(
                    (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt(),
                );
            }
            None => ln_med[l] = (prior.median[l] / 0.9).ln(),
        }
    }
    if !sigmas.is_empty() {
        sigmas.sort_by(|a, b| a.partial_cmp(b).unwrap());
        model.sigma = sigmas[sigmas.len() / 2].clamp(0.2, 0.9);
    }
    let mut ph = vec![(0.0, 0usize); bins];
    for (l, ln, b) in seq {
        ph[b].0 += ln - ln_med[l];
        ph[b].1 += 1;
    }
    model.phase = ph
        .iter()
        .zip(prior.phase.iter())
        .map(|(&(s, n), &p)| if n >= 20 { (s / n as f64).exp() } else { p })
        .collect();
    (model, by_match.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_options() {
        let some: Value = serde_json::json!([0, 7]);
        let none: Value = serde_json::json!([1, []]);
        assert_eq!(option_u64(&some), Some(7));
        assert_eq!(option_u64(&none), None);
    }

    #[test]
    fn refit_recovers_medians() {
        let mut auctions = Vec::new();
        let mut top = HashMap::new();
        let mut id = 0;
        for m in 0..30u64 {
            for i in 0..68 {
                id += 1;
                let letter = i % 26;
                auctions.push(LiveAuction {
                    auction_id: id,
                    match_id: m,
                    letter,
                    winner: Some(1),
                    paid: 5,
                });
                top.insert(id, 10 + (letter as i64 % 3));
            }
        }
        let h = LiveHistory {
            auctions,
            top_bid: top,
            all_real: (0..30).collect(),
        };
        let (f, n) = refit(&h, 100, &FieldModel::default());
        assert_eq!(n, 30);
        assert!((f.median[0] - 9.0).abs() < 1e-6); // 10 * 0.9
        assert!((f.median[1] - 9.9).abs() < 1e-6); // 11 * 0.9
    }
}
