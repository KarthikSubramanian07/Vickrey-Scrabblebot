//! Live A/B testing: the league itself decides between strategies.
//!
//! With `config/ab.json` present, casual matches are split between the
//! champion (`params.json`, arm `A`) and a challenger (arm `B:<file>`).
//! After every match the scoreboard is recomputed from `runs/summary.jsonl`
//! on Swiss points (`n − place`, what the tournament rewards). When both
//! arms have enough matches, a clear winner is acted on:
//!
//! * challenger significantly better → it becomes `params.json` (the old
//!   champion is kept as `params.prev.json`) and the test ends;
//! * challenger significantly worse → the test ends, champion unchanged.
//!
//! Every decision is appended to `runs/ab-history.jsonl`.

use crate::config::AbConfig;
use serde::Deserialize;
use std::io::Write;
use std::path::Path;

/// Minimum matches per arm before deciding.
pub const MIN_MATCHES: usize = 40;
/// |t| needed to act.
pub const T_DECIDE: f64 = 2.0;

#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    arm: Option<String>,
    place: usize,
    n_players: usize,
    our_score: i64,
    #[serde(default)]
    tournament_id: Option<u64>,
    #[serde(default)]
    resumed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArmStats {
    pub n: usize,
    pub mean_points: f64,
    pub var_points: f64,
    pub mean_place: f64,
    pub win_rate: f64,
    pub mean_score: f64,
}

fn stats(xs: &[(f64, f64, f64, f64)]) -> ArmStats {
    let n = xs.len();
    if n == 0 {
        return ArmStats::default();
    }
    let nf = n as f64;
    let mean = xs.iter().map(|x| x.0).sum::<f64>() / nf;
    let var = xs.iter().map(|x| (x.0 - mean).powi(2)).sum::<f64>() / (nf - 1.0).max(1.0);
    ArmStats {
        n,
        mean_points: mean,
        var_points: var,
        mean_place: xs.iter().map(|x| x.1).sum::<f64>() / nf,
        win_rate: xs.iter().map(|x| x.2).sum::<f64>() / nf,
        mean_score: xs.iter().map(|x| x.3).sum::<f64>() / nf,
    }
}

/// Welch t for B − A.
pub fn welch_t(a: &ArmStats, b: &ArmStats) -> f64 {
    let se = (a.var_points / a.n.max(1) as f64 + b.var_points / b.n.max(1) as f64).sqrt();
    if se <= 0.0 {
        0.0
    } else {
        (b.mean_points - a.mean_points) / se
    }
}

/// Champion and challenger scoreboards from the summary log: casual,
/// full-table, non-resumed matches only.
pub fn scoreboard(summary: &Path, ab: &AbConfig) -> (ArmStats, ArmStats) {
    let text = std::fs::read_to_string(summary).unwrap_or_default();
    let label_b = format!("B:{}", ab.challenger);
    let mut a = Vec::new();
    let mut b = Vec::new();
    let mut seen_b = false;
    for line in text.lines() {
        let Ok(l) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        if l.tournament_id.is_some() || l.resumed || l.n_players < 3 {
            continue;
        }
        let arm = l.arm.unwrap_or_default();
        let row = (
            (l.n_players - l.place) as f64,
            l.place as f64,
            (l.place == 1) as u8 as f64,
            l.our_score as f64,
        );
        // The test window opens with the challenger's first match; champion
        // matches before that ran under other conditions.
        if arm == label_b {
            seen_b = true;
            b.push(row);
        } else if arm == "A" && seen_b {
            a.push(row);
        }
    }
    (stats(&a), stats(&b))
}

fn log(runs: &Path, v: serde_json::Value) {
    let _ = std::fs::create_dir_all(runs);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(runs.join("ab-history.jsonl"))
    {
        let _ = writeln!(f, "{v}");
    }
}

/// Check the running test and act on a clear result. Returns a message when
/// something was decided.
pub fn maybe_decide(config_dir: &Path, runs_dir: &Path, ab: &AbConfig) -> Option<String> {
    let (a, b) = scoreboard(&runs_dir.join("summary.jsonl"), ab);
    if a.n < MIN_MATCHES || b.n < MIN_MATCHES {
        return None;
    }
    let t = welch_t(&a, &b);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let record = |verdict: &str| {
        serde_json::json!({
            "ts_ms": ts as u64, "verdict": verdict, "challenger": ab.challenger, "name": ab.name, "t": t,
            "a": {"n": a.n, "points": a.mean_points, "place": a.mean_place, "win": a.win_rate, "score": a.mean_score},
            "b": {"n": b.n, "points": b.mean_points, "place": b.mean_place, "win": b.win_rate, "score": b.mean_score},
        })
    };
    if t >= T_DECIDE {
        let champ = config_dir.join("params.json");
        let _ = std::fs::copy(&champ, config_dir.join("params.prev.json"));
        if std::fs::copy(config_dir.join(&ab.challenger), &champ).is_ok() {
            let _ = std::fs::remove_file(config_dir.join("ab.json"));
            log(runs_dir, record("promoted"));
            return Some(format!(
                "A/B: challenger {} promoted (t {t:+.2}; {:.2} vs {:.2} Swiss pts over {}/{} matches)",
                ab.challenger, b.mean_points, a.mean_points, b.n, a.n
            ));
        }
    } else if t <= -T_DECIDE {
        let _ = std::fs::remove_file(config_dir.join("ab.json"));
        log(runs_dir, record("rejected"));
        return Some(format!(
            "A/B: challenger {} rejected (t {t:+.2})",
            ab.challenger
        ));
    }
    None
}

/// `vickey ab-report`.
pub fn report(config_dir: &Path, runs_dir: &Path) {
    let Some(ab) = crate::config::load_ab(config_dir) else {
        println!("no A/B test running (config/ab.json absent)");
        if let Ok(h) = std::fs::read_to_string(runs_dir.join("ab-history.jsonl")) {
            println!("history:\n{h}");
        }
        return;
    };
    let (a, b) = scoreboard(&runs_dir.join("summary.jsonl"), &ab);
    println!(
        "A/B test '{}': champion params.json vs challenger {} ({}% of casual matches)",
        ab.name,
        ab.challenger,
        (ab.share * 100.0).round()
    );
    for (name, s) in [("A champion", &a), ("B challenger", &b)] {
        println!(
            "  {name:<13} n {:>4}  swiss pts {:.2}  place {:.2}  win {:>3.0}%  score {:.1}",
            s.n,
            s.mean_points,
            s.mean_place,
            s.win_rate * 100.0,
            s.mean_score
        );
    }
    println!(
        "  t (B − A) = {:+.2}; decides at |t| ≥ {T_DECIDE} with ≥ {MIN_MATCHES} matches per arm",
        welch_t(&a, &b)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, lines: &[(&str, usize)]) {
        let body: String = lines
            .iter()
            .map(|(arm, place)| {
                format!("{{\"arm\":\"{arm}\",\"place\":{place},\"n_players\":6,\"our_score\":80,\"resumed\":false}}\n")
            })
            .collect();
        std::fs::write(dir.join("summary.jsonl"), body).unwrap();
    }

    #[test]
    fn promotes_a_clearly_better_challenger() {
        let dir = std::env::temp_dir().join(format!("vickey-ab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("params.json"), "{\"min_play_len\": 7}").unwrap();
        std::fs::write(dir.join("b.json"), "{\"min_play_len\": 5}").unwrap();
        std::fs::write(dir.join("ab.json"), "{\"challenger\": \"b.json\"}").unwrap();
        let ab = crate::config::load_ab(&dir).unwrap();
        // B places 1–2, A places 4–5.
        let mut rows = Vec::new();
        for i in 0..50 {
            rows.push(("A", 4 + i % 2));
            rows.push(("B:b.json", 1 + i % 2));
        }
        write(&dir, &rows[..20]);
        assert!(
            maybe_decide(&dir, &dir, &ab).is_none(),
            "too few matches to decide"
        );
        write(&dir, &rows);
        let msg = maybe_decide(&dir, &dir, &ab).unwrap();
        assert!(msg.contains("promoted"), "{msg}");
        assert!(std::fs::read_to_string(dir.join("params.json"))
            .unwrap()
            .contains('5'));
        assert!(!dir.join("ab.json").exists());
        assert!(dir.join("params.prev.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
