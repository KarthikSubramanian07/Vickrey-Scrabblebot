//! Paired-comparison hill climbing over `Params`.
//!
//! A candidate and the incumbent sit at the *same* tables (same seeds, seats
//! rotated) so table luck cancels out, and we only adopt a candidate whose
//! edge is statistically clear on two independent batches.

use crate::agents::Agent;
use crate::arena::make_agent;
use crate::engine::{play_match, MatchConfig};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;
use serde::Serialize;
use vickey_brain::{FieldModel, Params};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Profile {
    /// Six-seat lobby/Swiss tables, scored by place.
    Multi,
    /// Heads-up final, scored by point difference.
    Duel,
}

/// Rivals drawn for multi-seat tables. `vickey@pop` seats run the incumbent's
/// params: the live league's best bots are valuation-driven too, and the
/// first live matches showed the scripted field alone is too soft.
const FIELD_FILL: [&str; 8] = [
    "shark",
    "shark",
    "grinder",
    "spender",
    "nibbler",
    "starter",
    "vickey@pop",
    "vickey@pop",
];

/// Mean and standard error of `cand − inc` per match.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Edge {
    pub mean: f64,
    pub se: f64,
    pub n: usize,
}

impl Edge {
    pub fn t(&self) -> f64 {
        if self.se <= 0.0 {
            0.0
        } else {
            self.mean / self.se
        }
    }

    fn from(xs: &[f64]) -> Edge {
        let n = xs.len().max(1) as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
        Edge {
            mean,
            se: (var / n).sqrt(),
            n: xs.len(),
        }
    }

    pub fn merge(a: Edge, b: Edge) -> Edge {
        let n = (a.n + b.n).max(1) as f64;
        let mean = (a.mean * a.n as f64 + b.mean * b.n as f64) / n;
        let se = ((a.se * a.n as f64).powi(2) + (b.se * b.n as f64).powi(2)).sqrt() / n;
        Edge {
            mean,
            se,
            n: a.n + b.n,
        }
    }
}

fn seat(spec: &str, cand: &Params, inc: &Params, field: &FieldModel, seed: u64) -> Box<dyn Agent> {
    match spec {
        "vickey@cand" => make_agent(spec, cand, field, seed),
        "vickey@inc" => make_agent(spec, inc, field, seed),
        _ => make_agent(spec, inc, field, seed),
    }
}

/// One paired sample: candidate's edge over the incumbent on seed `s`.
fn sample(
    profile: Profile,
    cand: &Params,
    inc: &Params,
    field: &FieldModel,
    cfg: &MatchConfig,
    s: u64,
) -> f64 {
    let mut rng = SmallRng::seed_from_u64(s);
    match profile {
        Profile::Multi => {
            let mut specs = vec!["vickey@cand".to_string(), "vickey@inc".to_string()];
            for _ in 0..4 {
                specs.push(FIELD_FILL[rng.gen_range(0..FIELD_FILL.len())].to_string());
            }
            let rot = (s % specs.len() as u64) as usize;
            specs.rotate_left(rot);
            let mut agents: Vec<Box<dyn Agent>> = specs
                .iter()
                .enumerate()
                .map(|(i, sp)| seat(sp, cand, inc, field, s ^ ((i as u64 + 1) * 0x51)))
                .collect();
            let r = play_match(&mut agents, cfg, s);
            let ic = specs.iter().position(|x| x == "vickey@cand").unwrap();
            let ii = specs.iter().position(|x| x == "vickey@inc").unwrap();
            // Swiss points by place, plus a small score tiebreak.
            let n = specs.len() as f64;
            (n - r.place(ic) as f64) - (n - r.place(ii) as f64)
                + 0.01 * (r.seats[ic].score - r.seats[ii].score) as f64
        }
        Profile::Duel => {
            // Head-to-head, both seat orders, plus each against a shark.
            let h2h = |order: [&str; 2], seed: u64| {
                let mut agents: Vec<Box<dyn Agent>> = order
                    .iter()
                    .enumerate()
                    .map(|(i, sp)| seat(sp, cand, inc, field, seed ^ (i as u64 + 7)))
                    .collect();
                let r = play_match(&mut agents, cfg, seed);
                let c = order.iter().position(|x| *x == "vickey@cand").unwrap();
                (r.seats[c].score - r.seats[1 - c].score) as f64
            };
            let vs_shark = |who: &str, seed: u64| {
                let mut agents: Vec<Box<dyn Agent>> = vec![
                    seat(who, cand, inc, field, seed ^ 3),
                    make_agent("shark", inc, field, seed ^ 5),
                ];
                let r = play_match(&mut agents, cfg, seed);
                (r.seats[0].score - r.seats[1].score) as f64
            };
            let a = if s.is_multiple_of(2) {
                h2h(["vickey@cand", "vickey@inc"], s)
            } else {
                h2h(["vickey@inc", "vickey@cand"], s)
            };
            let b = vs_shark("vickey@cand", s ^ 0xabc) - vs_shark("vickey@inc", s ^ 0xabc);
            (a + 0.5 * b) / 100.0
        }
    }
}

pub fn evaluate(
    profile: Profile,
    cand: &Params,
    inc: &Params,
    field: &FieldModel,
    cfg: &MatchConfig,
    n: u32,
    seed: u64,
) -> Edge {
    let xs: Vec<f64> = (0..n)
        .into_par_iter()
        .map(|i| {
            sample(
                profile,
                cand,
                inc,
                field,
                cfg,
                seed.wrapping_mul(0x9e3779b9).wrapping_add(i as u64),
            )
        })
        .collect();
    Edge::from(&xs)
}

/// Perturb 1–3 knobs of `p`.
pub fn mutate(p: &Params, profile: Profile, rng: &mut SmallRng) -> Params {
    let mut q = p.clone();
    let k = rng.gen_range(1..=3);
    for _ in 0..k {
        let lognorm =
            |x: f64, s: f64, rng: &mut SmallRng| x * (rng.gen_range(-1.0..1.0f64) * s).exp();
        let lin = |x: f64, s: f64, rng: &mut SmallRng| x + rng.gen_range(-1.0..1.0f64) * s;
        match rng.gen_range(0..13) {
            0 => q.income_factor = lin(q.income_factor, 0.4, rng).clamp(0.0, 3.0),
            1 => q.spend_target = lin(q.spend_target, 0.2, rng).clamp(0.5, 2.0),
            2 => q.choice_mult = lin(q.choice_mult, 0.4, rng).clamp(0.6, 3.5),
            3 => q.win_share_scale = lognorm(q.win_share_scale, 0.3, rng).clamp(0.3, 3.0),
            4 => q.max_future = (q.max_future as i64 + rng.gen_range(-6..=6)).clamp(4, 48) as usize,
            5 => q.lambda_min = lognorm(q.lambda_min, 0.6, rng).clamp(0.005, 0.5),
            6 => q.liquidity = lin(q.liquidity, 0.4, rng).clamp(0.0, 3.0),
            7 => q.play_margin = lin(q.play_margin, 1.5, rng).clamp(-3.0, 8.0),
            8 => {
                q.min_play_len =
                    (q.min_play_len as i64 + rng.gen_range(-1..=1)).clamp(3, 8) as usize
            }
            9 => q.cheap_grab_value = lognorm(q.cheap_grab_value, 0.5, rng).clamp(0.1, 5.0),
            11 if rng.gen_bool(0.3) => q.mu_liquidity = !q.mu_liquidity,
            10 if profile == Profile::Duel => {
                q.deny_weight = lin(q.deny_weight, 0.3, rng).clamp(0.0, 1.5)
            }
            _ => q.lambda_max = lognorm(q.lambda_max, 0.4, rng).clamp(0.5, 10.0),
        }
    }
    q
}

#[derive(Serialize)]
pub struct Step {
    pub iter: u32,
    pub accepted: bool,
    pub edge: Edge,
    pub params: Params,
}

/// Hill-climb from `start`. Calls `on_step` after each candidate so callers
/// can persist progress as it happens.
#[allow(clippy::too_many_arguments)]
pub fn tune(
    profile: Profile,
    start: Params,
    field: &FieldModel,
    cfg: &MatchConfig,
    iters: u32,
    matches: u32,
    seed: u64,
    on_step: &mut dyn FnMut(&Step, &Params),
) -> Params {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut inc = start;
    for it in 0..iters {
        let cand = mutate(&inc, profile, &mut rng);
        if cand == inc {
            continue;
        }
        let e1 = evaluate(profile, &cand, &inc, field, cfg, matches, rng.gen());
        let mut accepted = false;
        let mut edge = e1;
        if e1.t() > 1.3 {
            let e2 = evaluate(profile, &cand, &inc, field, cfg, matches, rng.gen());
            edge = Edge::merge(e1, e2);
            if e2.mean > 0.0 && edge.t() > 2.0 {
                inc = cand.clone();
                accepted = true;
            }
        }
        on_step(
            &Step {
                iter: it,
                accepted,
                edge,
                params: cand,
            },
            &inc,
        );
    }
    inc
}
