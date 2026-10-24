//! Every tunable knob, serialisable so the tuner can evolve them and the
//! live bot can hot-reload them between matches.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Params {
    // ---- valuation ----
    /// Monte Carlo futures per letter valuation.
    pub samples: usize,
    /// Futures for the cheap all-letters table used by pacing.
    pub table_samples: usize,
    /// Node budget per terminal solve inside the Monte Carlo (0 = greedy).
    pub solve_nodes: u64,
    /// Scales how many future letters we expect to win.
    pub win_share_scale: f64,
    /// Extra letters sampled per expected win, standing in for our freedom
    /// to pick which letters to buy.
    pub choice_mult: f64,
    /// Cap on future letters assumed.
    pub max_future: usize,

    // ---- pacing / bidding ----
    /// Future word income counted as budget, as a share of current balance
    /// times the share of the match left.
    pub income_factor: f64,
    /// Floor on λ (points per coin) — the most aggressive we ever get.
    pub lambda_min: f64,
    pub lambda_max: f64,
    /// Fraction of the remaining auctions' budget to spend (slack < 1 keeps
    /// a reserve, > 1 front-loads).
    pub spend_target: f64,
    /// Below this marginal value (points) we skip the letter.
    pub min_value: f64,
    /// Bid at least 1 coin on any letter worth this many points (reserve price).
    pub cheap_grab_value: f64,
    /// First-price: grid search the bid that maximises surplus.
    pub first_price_shade: bool,
    /// Weight on denying the strongest rival (relative-score games).
    pub deny_weight: f64,
    /// Denial only kicks in at tables this small or smaller.
    pub deny_max_players: usize,

    // ---- play ----
    /// Never play a word shorter than this mid-match (the final dump plays everything).
    pub min_play_len: usize,
    /// Value of playing now: liquidity credit per point, times λ.
    pub liquidity: f64,
    /// A mid-match play must beat holding by this many points.
    pub play_margin: f64,
    /// How many top words to consider per play decision.
    pub play_candidates: usize,
    /// Milliseconds for the final exact partition.
    pub final_solve_ms: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            samples: 48,
            table_samples: 10,
            solve_nodes: 0,
            win_share_scale: 1.0,
            choice_mult: 1.6,
            max_future: 24,
            income_factor: 0.8,
            lambda_min: 0.05,
            lambda_max: 4.0,
            spend_target: 1.0,
            min_value: 0.3,
            cheap_grab_value: 0.8,
            first_price_shade: true,
            deny_weight: 0.5,
            deny_max_players: 2,
            min_play_len: 6,
            liquidity: 0.6,
            play_margin: 0.5,
            play_candidates: 10,
            final_solve_ms: 180,
        }
    }
}

impl Params {
    pub fn load(path: &Path) -> anyhow_lite::Result<Params> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, path: &Path) -> anyhow_lite::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)? + "\n")?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

/// Minimal boxed-error result so the brain stays dependency-light.
pub mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
}
