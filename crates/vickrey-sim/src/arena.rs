//! Lineups, batch evaluation and summary statistics.

use crate::agents::{
    Agent, CloneKind, FieldBot, LiveClone, ServerSim, SimStrategy, Starter, Vickrey,
};
use crate::engine::{play_match, MatchConfig};
use rayon::prelude::*;
use std::collections::BTreeMap;
use vickrey_brain::{FieldModel, Params};

/// Build an agent from a short spec:
/// `vickrey`, `vickrey@<label>` (uses `params`), `shark`, `grinder`, `spender`,
/// `nibbler`, `field`, `starter`, `cheapo`, `valor`, `brutus`.
pub fn make_agent(spec: &str, params: &Params, field: &FieldModel, seed: u64) -> Box<dyn Agent> {
    let (kind, label) = spec.split_once('@').unwrap_or((spec, spec));
    match kind {
        "vickrey" => Box::new(Vickrey::new(label, params.clone(), field.clone(), seed)),
        "starter" => Box::new(Starter::new(label)),
        "cheapo" => Box::new(ServerSim::new(SimStrategy::Cheapskate, label, seed)),
        "valor" => Box::new(ServerSim::new(SimStrategy::ValueBidder, label, seed)),
        "brutus" => Box::new(ServerSim::new(SimStrategy::Aggressive, label, seed)),
        "ramp" => Box::new(LiveClone::new(label, CloneKind::Ramp, seed)),
        "flat" => Box::new(LiveClone::new(label, CloneKind::Flat, seed)),
        other => Box::new(FieldBot::preset_with(other, seed, field.clone())),
    }
}

#[derive(Clone, Debug, Default)]
pub struct SeatStats {
    pub matches: u32,
    pub score_sum: f64,
    pub score_sq: f64,
    pub place_sum: f64,
    pub wins: u32,
    pub swiss_points: f64,
    pub letters: f64,
    pub spent: f64,
    pub leftover: f64,
}

impl SeatStats {
    pub fn mean_score(&self) -> f64 {
        self.score_sum / self.matches.max(1) as f64
    }
    pub fn stderr_score(&self) -> f64 {
        let n = self.matches.max(2) as f64;
        let m = self.mean_score();
        ((self.score_sq / n - m * m).max(0.0) / (n - 1.0)).sqrt()
    }
    pub fn mean_place(&self) -> f64 {
        self.place_sum / self.matches.max(1) as f64
    }
    pub fn win_rate(&self) -> f64 {
        self.wins as f64 / self.matches.max(1) as f64
    }
}

/// One evaluated match, per seat: (label, score, place, letters, spent, leftover).
pub type SeatLine = (String, i64, usize, u32, i64, i64);

/// Run `n` matches of `lineup` (seat order rotated each match) in parallel.
pub fn run_lineup(
    lineup: &[String],
    params_by_label: &(dyn Fn(&str) -> Params + Sync),
    field: &FieldModel,
    cfg: &MatchConfig,
    n: u32,
    seed: u64,
) -> BTreeMap<String, SeatStats> {
    let per_match: Vec<Vec<SeatLine>> = (0..n)
        .into_par_iter()
        .map(|m| {
            let s = seed.wrapping_mul(1_000_003).wrapping_add(m as u64);
            let k = lineup.len();
            let rot = (m as usize) % k;
            let specs: Vec<&String> = (0..k).map(|i| &lineup[(i + rot) % k]).collect();
            let mut agents: Vec<Box<dyn Agent>> = specs
                .iter()
                .enumerate()
                .map(|(i, spec)| {
                    let label = spec.split_once('@').map(|x| x.1).unwrap_or(spec);
                    make_agent(
                        spec,
                        &params_by_label(label),
                        field,
                        s ^ ((i as u64 + 1) * 0x9e37),
                    )
                })
                .collect();
            let r = play_match(&mut agents, cfg, s);
            (0..k)
                .map(|i| {
                    let st = &r.seats[i];
                    (
                        st.name.clone(),
                        st.score,
                        r.place(i),
                        st.letters_won,
                        st.spent,
                        st.balance,
                    )
                })
                .collect()
        })
        .collect();
    let mut stats: BTreeMap<String, SeatStats> = BTreeMap::new();
    for lines in per_match {
        let k = lines.len();
        for (name, score, place, letters, spent, leftover) in lines {
            let e = stats.entry(name).or_default();
            e.matches += 1;
            e.score_sum += score as f64;
            e.score_sq += (score * score) as f64;
            e.place_sum += place as f64;
            e.wins += (place == 1) as u32;
            e.swiss_points += (k - place) as f64;
            e.letters += letters as f64;
            e.spent += spent as f64;
            e.leftover += leftover as f64;
        }
    }
    stats
}

pub fn print_stats(stats: &BTreeMap<String, SeatStats>) {
    println!(
        "{:<18} {:>6} {:>8} {:>6} {:>7} {:>6} {:>7} {:>7} {:>8}",
        "seat", "n", "score", "±se", "place", "win%", "letters", "spent", "leftover"
    );
    let mut rows: Vec<_> = stats.iter().collect();
    rows.sort_by(|a, b| a.1.mean_place().partial_cmp(&b.1.mean_place()).unwrap());
    for (name, s) in rows {
        let n = s.matches.max(1) as f64;
        println!(
            "{:<18} {:>6} {:>8.1} {:>6.1} {:>7.2} {:>5.0}% {:>7.1} {:>7.1} {:>8.1}",
            name,
            s.matches,
            s.mean_score(),
            s.stderr_score(),
            s.mean_place(),
            100.0 * s.win_rate(),
            s.letters / n,
            s.spent / n,
            s.leftover / n
        );
    }
}
