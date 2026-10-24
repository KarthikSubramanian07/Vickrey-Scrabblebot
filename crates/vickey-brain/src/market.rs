//! What the rest of the table will bid.
//!
//! The highest opposing bid on a letter is modelled as log-normal around a
//! per-letter median (fitted to the live server's history), scaled by the
//! match phase and by a "heat" factor learned online from the prices this
//! table actually pays. It can never exceed the richest opponent's balance,
//! which is public, so the distribution is truncated there.

use serde::{Deserialize, Serialize};
use vickey_core::{Letter, ALPHABET};

/// Priors for the field, refit by `vickey-lab` from live data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldModel {
    /// Median of the highest bid among the other bots, per letter, at
    /// a full table of `ref_players` with normal wealth.
    pub median: [f64; ALPHABET],
    /// Log-scale spread.
    pub sigma: f64,
    /// Multipliers over the match, evenly spaced from first to last auction.
    pub phase: Vec<f64>,
    /// Probability that no opponent bids at all.
    pub p_none: f64,
    pub ref_players: usize,
}

impl Default for FieldModel {
    fn default() -> Self {
        // Fitted on 287 all-real 6-bot matches (Oct 24 2026). The median of the
        // max of five rivals sits between the observed top and second bids.
        let top: [f64; ALPHABET] = [
            13.0, 19.0, 20.0, 16.0, 14.0, 21.0, 15.0, 24.0, 13.0, 32.0, 27.0, 13.0, 20.0, 13.0,
            13.0, 20.0, 33.0, 14.0, 16.0, 13.0, 13.0, 20.0, 20.0, 35.0, 23.0, 43.0,
        ];
        let mut median = [0.0; ALPHABET];
        for l in 0..ALPHABET {
            median[l] = top[l] * 0.9;
        }
        FieldModel {
            median,
            sigma: 0.42,
            phase: vec![
                1.346, 1.247, 1.126, 1.054, 1.003, 0.962, 0.948, 0.933, 0.913, 0.93, 0.939, 0.962,
                0.951, 0.966, 1.188, 1.114, 0.855,
            ],
            p_none: 0.01,
            ref_players: 6,
        }
    }
}

impl FieldModel {
    pub fn phase_factor(&self, progress: f64) -> f64 {
        if self.phase.is_empty() {
            return 1.0;
        }
        let x = progress.clamp(0.0, 1.0) * (self.phase.len() - 1) as f64;
        let i = x.floor() as usize;
        let j = (i + 1).min(self.phase.len() - 1);
        let t = x - i as f64;
        self.phase[i] * (1.0 - t) + self.phase[j] * t
    }
}

/// Standard normal CDF (Abramowitz–Stegun 7.1.26 via erf).
pub fn norm_cdf(z: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * z.abs() / std::f64::consts::SQRT_2);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-(z * z) / 2.0).exp();
    if z >= 0.0 {
        0.5 * (1.0 + y)
    } else {
        0.5 * (1.0 - y)
    }
}

/// The opposing-bid distribution for one auction.
#[derive(Clone, Copy, Debug)]
pub struct BidDist {
    pub median: f64,
    pub sigma: f64,
    /// Nobody can bid more than this (richest opponent's balance).
    pub cap: f64,
    pub p_none: f64,
}

impl BidDist {
    /// P(highest opposing bid < b): we win outright with bid `b`
    /// (ties are coin flips; counted as half).
    pub fn p_win(&self, b: f64) -> f64 {
        if b <= 0.0 {
            return 0.0;
        }
        if self.cap <= 0.0 {
            return 1.0;
        }
        let below = |x: f64| -> f64 {
            if x >= self.cap {
                return 1.0;
            }
            if x <= 0.0 {
                return 0.0;
            }
            norm_cdf((x / self.median).ln() / self.sigma)
        };
        // Integer bids: P(M < b) + 0.5 P(M == b), M on integers.
        let lt = below(b - 0.5);
        let le = below(b + 0.5);
        let p = lt + 0.5 * (le - lt);
        self.p_none + (1.0 - self.p_none) * p
    }

    /// Expected payment under Vickrey with bid `b`: E[M · 1{M < b}].
    pub fn expected_pay_vickrey(&self, b: f64) -> f64 {
        if b <= 0.0 {
            return 0.0;
        }
        let top = b.min(self.cap + 1.0).floor() as i64;
        let mut e = 0.0;
        let mut prev = self.p_win_strict(1.0);
        for m in 1..top.max(1) {
            let next = self.p_win_strict(m as f64 + 1.0);
            e += m as f64 * (next - prev);
            prev = next;
        }
        // Lone bidder pays the reserve of 1.
        e + self.p_none
    }

    fn p_win_strict(&self, b: f64) -> f64 {
        if b <= 0.0 {
            return 0.0;
        }
        let x = b - 0.5;
        if x >= self.cap {
            return 1.0;
        }
        norm_cdf((x / self.median).ln() / self.sigma)
    }
}

/// Online price tracker for one match.
#[derive(Clone, Debug)]
pub struct Market {
    pub field: FieldModel,
    /// Multiplicative correction learned from this table's clearing prices.
    pub heat: f64,
    obs: u32,
}

impl Market {
    pub fn new(field: FieldModel) -> Self {
        Market {
            field,
            heat: 1.0,
            obs: 0,
        }
    }

    /// Size correction: fewer rivals means a lower maximum.
    fn table_factor(n_opponents: usize, ref_players: usize) -> f64 {
        let n = n_opponents.max(1) as f64;
        let r = (ref_players.max(2) - 1) as f64;
        // Max of n log-normals grows roughly with sqrt(2 ln n).
        let g = |k: f64| 1.0 + 0.25 * (2.0 * k.ln()).max(0.0).sqrt();
        g(n) / g(r)
    }

    pub fn dist(&self, letter: Letter, progress: f64, n_opponents: usize, cap: i64) -> BidDist {
        let m = self.field.median[letter]
            * self.field.phase_factor(progress)
            * self.heat
            * Self::table_factor(n_opponents, self.field.ref_players);
        BidDist {
            median: m.max(0.5),
            sigma: self.field.sigma,
            cap: cap.max(0) as f64,
            p_none: self.field.p_none,
        }
    }

    /// Feed a clearing price. When someone else won, `paid` was the
    /// second-highest bid overall; when we won it is the highest opposing bid.
    pub fn observe(
        &mut self,
        letter: Letter,
        progress: f64,
        paid: i64,
        we_won: bool,
        n_opponents: usize,
    ) {
        if paid <= 1 {
            return;
        }
        let pred = self.field.median[letter]
            * self.field.phase_factor(progress)
            * Self::table_factor(n_opponents, self.field.ref_players);
        // A second price sits below the opposing maximum; correct for it.
        let adj = if we_won { 1.0 } else { 1.0 / 0.8 };
        let ratio = (paid as f64 * adj / pred).clamp(0.2, 5.0);
        self.obs += 1;
        let w = (1.0 / (self.obs as f64 + 3.0)).max(0.12);
        self.heat = (self.heat.ln() * (1.0 - w) + ratio.ln() * w).exp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_probability_is_monotone_and_capped() {
        let d = BidDist {
            median: 12.0,
            sigma: 0.4,
            cap: 30.0,
            p_none: 0.0,
        };
        let mut prev = 0.0;
        for b in 1..40 {
            let p = d.p_win(b as f64);
            assert!(p >= prev - 1e-12);
            prev = p;
        }
        assert!((d.p_win(31.0) - 1.0).abs() < 1e-9);
        assert!((d.p_win(12.0) - 0.5).abs() < 0.08);
    }

    #[test]
    fn expected_pay_grows_with_bid() {
        let d = BidDist {
            median: 12.0,
            sigma: 0.4,
            cap: 100.0,
            p_none: 0.0,
        };
        assert!(d.expected_pay_vickrey(20.0) > d.expected_pay_vickrey(10.0));
        assert!(d.expected_pay_vickrey(20.0) < 20.0);
    }

    #[test]
    fn heat_tracks_expensive_tables() {
        let mut m = Market::new(FieldModel::default());
        for _ in 0..20 {
            m.observe(0, 0.5, 30, true, 5);
        }
        assert!(m.heat > 1.8);
    }
}
