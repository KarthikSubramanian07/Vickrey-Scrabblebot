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
    /// Blend of the static letter prior into the pacing table (0 = pure Monte Carlo).
    pub table_shrink: f64,
    /// Work cap per valuation (letter² units, ~1.7 µs each); bounds latency
    /// on huge racks. The main valuation and the table each get this much.
    pub work_budget: u64,
    /// Racks this small (or smaller) are valued with the exact solver
    /// instead of greedy (0 = off).
    pub exact_terminal_max: usize,
    /// Node budget per terminal solve inside the Monte Carlo (0 = greedy).
    pub solve_nodes: u64,
    /// Scales how many future letters we expect to win.
    pub win_share_scale: f64,
    /// Extra letters sampled per expected win, standing in for our freedom
    /// to pick which letters to buy.
    pub choice_mult: f64,
    /// Premium letters (J, K, Q, X, Z) are valued against this many times
    /// more future letters. A premium needs specific partners, which a short
    /// sampled future rarely contains, so its marginal value collapses in the
    /// second half; the live leaders win 42% of second-half premium auctions
    /// to our 3%. 1.0 disables.
    pub premium_choice_mult: f64,
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
    /// Price each future stretch of the match at its own phase when pacing
    /// (hot opening, cheap close) instead of everything at today's prices.
    pub phase_aware_pacing: bool,
    /// Bid this fraction under value during the first quarter (0 = off).
    pub opening_shade: f64,
    /// Below this marginal value (points) we skip the letter.
    pub min_value: f64,
    /// Bid at least 1 coin on any letter worth this many points (reserve price).
    pub cheap_grab_value: f64,
    /// First-price: grid search the bid that maximises surplus.
    pub first_price_shade: bool,
    /// Bid at least this share of the market median on ordinary letters
    /// worth `volume_min_value`+ points to us (0 = off).
    pub volume_floor: f64,
    pub volume_min_value: f64,
    /// In the last N usable auctions, bid the whole balance on any letter
    /// worth at least `endgame_allin_min_value` points (0 = off).
    pub endgame_allin_auctions: u32,
    pub endgame_allin_min_value: f64,
    /// Cap bids on J/K/Q/X/Z at this multiple of the market median (0 = off).
    pub premium_cap_mult: f64,
    /// Apply the premium cap only while match progress is below this (1.0 = always).
    pub premium_cap_until: f64,
    /// Bid this fraction over value when the letter completes a 7+ word now
    /// and is worth at least `completion_min_value` points (0 = off).
    pub completion_premium: f64,
    pub completion_min_value: f64,
    /// Wall-clock cap on a bid's Monte Carlo sampling (0 = none). Off by
    /// default so valuation is deterministic in tests and the simulator; the
    /// live config sets it, since a late bid is worth nothing.
    pub bid_deadline_ms: u64,
    /// Weight on denying the strongest rival (relative-score games).
    pub deny_weight: f64,
    /// Denial only kicks in at tables this small or smaller.
    pub deny_max_players: usize,

    // ---- play ----
    /// Never play a word shorter than this mid-match (the final dump plays everything).
    pub min_play_len: usize,
    /// Mid-match words paying less than this are held for a longer, richer
    /// word unless we are starved or in the late window. The live leaders
    /// (james, Waupus, Lambi Lambiii) make 84-86% of their mid-match plays at
    /// 45+ points or when broke, first play around auction 36-40, and earn
    /// 32-36 points in the last quarter; we cash at ~40 points around
    /// auction 20 and earn 12.7 there. 0 disables.
    pub cash_min_reward: u32,
    /// Value of playing now: liquidity credit per point, times λ.
    pub liquidity: f64,
    /// Price liquidity with μ (marginal points per coin under the balance
    /// cap) instead of the pacing λ. Off by default: neutral-to-slightly
    /// worse in simulation; the tuner may switch it on.
    pub mu_liquidity: bool,
    /// With this many tiles (or fewer) left in the bag, cash words as short
    /// as `late_min_play_len`, pricing coins with μ. 0 disables.
    pub late_window: u32,
    pub late_min_play_len: usize,
    /// Below this balance (with 3+ usable auctions left) we are "starved":
    /// cash words as short as `starve_min_play_len`, pricing coins with μ.
    pub starve_coins: i64,
    pub starve_min_play_len: usize,
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
            table_shrink: 0.5,
            work_budget: 70_000,
            exact_terminal_max: 0,
            solve_nodes: 0,
            win_share_scale: 1.0,
            choice_mult: 1.6,
            premium_choice_mult: 1.0,
            max_future: 24,
            income_factor: 0.8,
            lambda_min: 0.05,
            lambda_max: 4.0,
            spend_target: 1.0,
            phase_aware_pacing: false,
            opening_shade: 0.0,
            min_value: 0.3,
            cheap_grab_value: 0.8,
            first_price_shade: true,
            volume_floor: 0.0,
            volume_min_value: 2.0,
            endgame_allin_auctions: 0,
            endgame_allin_min_value: 3.0,
            premium_cap_mult: 0.0,
            premium_cap_until: 1.0,
            completion_premium: 0.0,
            completion_min_value: 8.0,
            bid_deadline_ms: 0,
            deny_weight: 0.5,
            deny_max_players: 2,
            min_play_len: 6,
            cash_min_reward: 0,
            liquidity: 0.6,
            mu_liquidity: false,
            late_window: 0,
            starve_coins: 15,
            starve_min_play_len: 5,
            late_min_play_len: 4,
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
