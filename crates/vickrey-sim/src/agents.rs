//! Players for the simulator: exact copies of the server's sim bots and
//! the starter kit, field models calibrated to the live leaderboard, and
//! our own brain.

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use vickrey_brain::{Brain, FieldModel, Params, Tracker};
use vickrey_core::letters::{VALUE, VOWELS};
use vickrey_core::{dictionary, Letter, Rack, Solver};

/// One seat at the table. The engine keeps each agent's `Tracker` current
/// before every call.
pub trait Agent: Send {
    fn name(&self) -> String;
    fn start(&mut self, _t: &Tracker) {}
    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64;
    fn observe(&mut self, _t: &Tracker, _letter: Letter, _winner: Option<u64>, _paid: i64) {}
    /// Words to play during the open auction (mid-match).
    fn plays(&mut self, t: &Tracker) -> Vec<String>;
    /// Words to play once the last auction opens.
    fn final_plays(&mut self, t: &Tracker) -> Vec<String>;
    /// Server-side sim bots act inside the module: they play right after
    /// winning, even on the last auction.
    fn is_server_sim(&self) -> bool {
        false
    }
}

// ------------------------------------------------------------ server sims

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimStrategy {
    Cheapskate,
    ValueBidder,
    Aggressive,
}

/// The module's built-in bots (`game.ts: decideBid` / `simulateWordPlay`).
pub struct ServerSim {
    pub strategy: SimStrategy,
    pub label: String,
    rng: SmallRng,
}

impl ServerSim {
    pub fn new(strategy: SimStrategy, label: &str, seed: u64) -> Self {
        ServerSim {
            strategy,
            label: label.to_string(),
            rng: SmallRng::seed_from_u64(seed),
        }
    }
}

impl Agent for ServerSim {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        let v = VALUE[letter] as i64;
        let base = match self.strategy {
            SimStrategy::Cheapskate => v - 1,
            SimStrategy::ValueBidder => v,
            SimStrategy::Aggressive => v + if VOWELS.contains(&letter) { 4 } else { 2 },
        };
        let jitter = self.rng.gen_range(-2..=2);
        (base + jitter).max(1).min(t.my_balance()).max(0)
    }

    fn plays(&mut self, _t: &Tracker) -> Vec<String> {
        Vec::new()
    }

    fn final_plays(&mut self, _t: &Tracker) -> Vec<String> {
        Vec::new()
    }

    fn is_server_sim(&self) -> bool {
        true
    }
}

/// What a server sim plays after a win: the longest word of 3+ letters.
pub fn server_sim_play(rack: &Rack) -> Option<String> {
    dictionary().longest_word(rack, 3).map(str::to_string)
}

// ------------------------------------------------------------ starter kit

/// `bot-starter/src/strategy.ts` as shipped: face value (+2 if we lack the
/// letter), and the longest playable word whenever the rack changes.
pub struct Starter {
    label: String,
}

impl Starter {
    pub fn new(label: &str) -> Self {
        Starter {
            label: label.to_string(),
        }
    }
}

impl Agent for Starter {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        let v = VALUE[letter] as i64;
        let want = if t.my_rack.count(letter) == 0 {
            v + 2
        } else {
            v.max(1)
        };
        want.min(t.my_balance().max(0))
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        // Debounced to ~2 attempts per 1 s window.
        let mut r = t.my_rack;
        let mut out = Vec::new();
        for _ in 0..2 {
            match dictionary().longest_word(&r, 2) {
                Some(w) => {
                    r.sub(&Rack::from_word(w));
                    out.push(w.to_string());
                }
                None => break,
            }
        }
        out
    }

    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        self.plays(t)
    }
}

// ------------------------------------------------------------ field bots

/// A parametric stand-in for the live competition, calibrated so a table of
/// them reproduces the live price levels (letters clearing ~10–33 coins)
/// and scores (winner ≈ 107).
#[derive(Clone, Debug)]
pub struct FieldStyle {
    /// Multiplier on the fitted market median.
    pub aggression: f64,
    /// Log-normal noise on bids.
    pub noise: f64,
    /// How bids scale with balance / 100.
    pub wealth_exp: f64,
    /// Only play words at least this long mid-match.
    pub min_len: usize,
    /// Dump the exact best partition at the end (otherwise greedy-longest).
    pub smart_dump: bool,
    /// Bias towards letters that fit the rack (vowel/consonant balance).
    pub fit_bias: f64,
    /// Multiplier on bids for premium letters (J, K, Q, X, Z).
    pub premium_factor: f64,
}

pub struct FieldBot {
    label: String,
    style: FieldStyle,
    field: FieldModel,
    rng: SmallRng,
}

impl FieldBot {
    pub fn new(label: &str, style: FieldStyle, seed: u64) -> Self {
        FieldBot {
            label: label.to_string(),
            style,
            field: FieldModel::default(),
            rng: SmallRng::seed_from_u64(seed),
        }
    }

    /// Named presets spanning the live leaderboard, pricing from `field`
    /// (the same fitted model the brain uses, so both sides see one market).
    pub fn preset_with(kind: &str, seed: u64, field: FieldModel) -> Self {
        let mut b = Self::preset(kind, seed);
        b.field = field;
        b
    }

    /// Named presets spanning the live leaderboard.
    pub fn preset(kind: &str, seed: u64) -> Self {
        let style = match kind {
            // Top live bots: pay up, hoard for 7+, dump optimally.
            "shark" => FieldStyle {
                aggression: 1.05,
                noise: 0.25,
                wealth_exp: 0.5,
                min_len: 7,
                smart_dump: true,
                fit_bias: 0.6,
                premium_factor: 1.0,
            },
            // Clone of the live league's top bot (postturing): wins ~15 letters a
            // match at ~11.6 coins by bidding market rate on common letters and
            // under half the market on premium ones, and cashes 5-6 letter
            // words mid-match to keep coins flowing.
            "volume" => FieldStyle {
                aggression: 1.0,
                noise: 0.25,
                wealth_exp: 0.5,
                min_len: 5,
                smart_dump: true,
                fit_bias: 0.5,
                premium_factor: 0.55,
            },
            // Mid-table: plays 5+ letter words as they appear.
            "grinder" => FieldStyle {
                aggression: 0.95,
                noise: 0.3,
                wealth_exp: 0.4,
                min_len: 5,
                smart_dump: true,
                fit_bias: 0.3,
                premium_factor: 1.0,
            },
            // Overbidder that burns money early.
            "spender" => FieldStyle {
                aggression: 1.35,
                noise: 0.35,
                wealth_exp: 0.8,
                min_len: 6,
                smart_dump: false,
                fit_bias: 0.2,
                premium_factor: 1.0,
            },
            // Cautious, plays anything 4+.
            "nibbler" => FieldStyle {
                aggression: 0.75,
                noise: 0.3,
                wealth_exp: 0.3,
                min_len: 4,
                smart_dump: false,
                fit_bias: 0.2,
                premium_factor: 1.0,
            },
            _ => FieldStyle {
                aggression: 1.0,
                noise: 0.3,
                wealth_exp: 0.5,
                min_len: 6,
                smart_dump: true,
                fit_bias: 0.4,
                premium_factor: 1.0,
            },
        };
        FieldBot::new(kind, style, seed)
    }

    fn fit(&self, rack: &Rack, letter: Letter) -> f64 {
        // Crude rack balance: vowels are wanted when short of them.
        let n = rack.len().max(1) as f64;
        let vowels: f64 = VOWELS.iter().map(|&v| rack.count(v) as f64).sum();
        let ratio = vowels / n;
        let is_vowel = VOWELS.contains(&letter);
        let have = rack.count(letter) as f64;
        let balance_term = if is_vowel { 0.4 - ratio } else { ratio - 0.4 };
        (1.0 + self.style.fit_bias * (balance_term * 2.0 - 0.25 * have)).clamp(0.4, 1.6)
    }
}

impl Agent for FieldBot {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        if t.is_last_auction() {
            return 0;
        }
        let bal = t.my_balance().max(0) as f64;
        let wealth = (bal / 100.0).max(0.05).powf(self.style.wealth_exp);
        let z: f64 = self.rng.sample(rand_distr_normal());
        let m = self.field.median[letter] * self.field.phase_factor(t.progress()) * 0.95;
        let b = m
            * self.style.aggression
            * wealth
            * (self.style.noise * z).exp()
            * self.fit(&t.my_rack, letter);
        (b.round() as i64).clamp(0, t.my_balance().max(0))
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        let d = dictionary();
        let mut r = t.my_rack;
        let mut out = Vec::new();
        while let Some(s) = d.best_word(&r, self.style.min_len) {
            r.sub(&s.rack);
            out.push(s.word.to_string());
        }
        out
    }

    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        let d = dictionary();
        if self.style.smart_dump {
            let mut s = Solver::with_budget(d, 200_000);
            s.solve(&t.my_rack)
                .word_strings(d)
                .into_iter()
                .map(str::to_string)
                .collect()
        } else {
            let mut r = t.my_rack;
            let mut out = Vec::new();
            while let Some(w) = d.longest_word(&r, 2) {
                r.sub(&Rack::from_word(w));
                out.push(w.to_string());
            }
            out
        }
    }
}

/// Standard normal via Box–Muller (keeps us off `rand_distr`).
fn rand_distr_normal() -> impl rand::distributions::Distribution<f64> {
    struct N;
    impl rand::distributions::Distribution<f64> for N {
        fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> f64 {
            let u1: f64 = rng.gen_range(1e-12..1.0);
            let u2: f64 = rng.gen();
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        }
    }
    N
}

// ------------------------------------------------------------ live clones

/// Which fitted live family a [`LiveClone`] imitates. Constants refit on
/// 1,568 live matches (ids 2732–4299) on tournament eve; see
/// `runs/research/proposals.md` (rival watch 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneKind {
    /// Doppio / Nijello / Heidi / benx pool: hoard early, ramp bids with
    /// match progress and balance, spend most of the bank in the last
    /// quarter, dump at the end. `−2.5 + 2.8·face + 15·progress +
    /// 11·balance/100`, residual sd 6.2.
    Ramp,
    /// guesser / postturing / atlas / Chinese Dante pool: `7 + 2.75·face +
    /// 3·progress + 4.3·balance/100`, sd 4.7; plays 5+ letter words on sight.
    Flat,
    /// Metheny-style face hunter: `4.4 + 3.94·face + 7.1·progress +
    /// 3.2·balance/100`, sd 4.3; cashes 6+ letter words on sight.
    Hunter,
}

/// A rival reproduced from regression fits of the live league's top bots
/// (`runs/research/rivals.md`, section 6).
pub struct LiveClone {
    label: String,
    kind: CloneKind,
    rng: SmallRng,
    /// Ramp only: the chance per auction of cashing a 6+ letter word mid-match
    /// (Doppio plays ~0.5 such words per match).
    mid_cash: f64,
}

impl LiveClone {
    pub fn new(label: &str, kind: CloneKind, seed: u64) -> Self {
        LiveClone {
            label: label.to_string(),
            kind,
            rng: SmallRng::seed_from_u64(seed),
            mid_cash: if kind == CloneKind::Ramp { 0.012 } else { 0.0 },
        }
    }
}

impl Agent for LiveClone {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        if t.is_last_auction() {
            return 0;
        }
        let face = VALUE[letter] as f64;
        let z: f64 = self.rng.sample(rand_distr_normal());
        let b = match self.kind {
            CloneKind::Ramp => {
                let bal = t.my_balance().max(0) as f64;
                -2.5 + 2.8 * face + 15.0 * t.progress() + 11.0 * bal / 100.0 + 6.2 * z
            }
            CloneKind::Flat => {
                let bal = t.my_balance().max(0) as f64;
                7.0 + 2.75 * face + 3.0 * t.progress() + 4.3 * bal / 100.0 + 4.7 * z
            }
            CloneKind::Hunter => {
                let bal = t.my_balance().max(0) as f64;
                4.4 + 3.94 * face + 7.1 * t.progress() + 3.2 * bal / 100.0 + 4.3 * z
            }
        };
        (b.round() as i64).clamp(0, t.my_balance().max(0))
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        let d = dictionary();
        let mut r = t.my_rack;
        let mut out = Vec::new();
        match self.kind {
            CloneKind::Flat | CloneKind::Hunter => {
                let min_len = if self.kind == CloneKind::Flat { 5 } else { 6 };
                while let Some(s) = d.best_word(&r, min_len) {
                    r.sub(&s.rack);
                    out.push(s.word.to_string());
                }
            }
            CloneKind::Ramp => {
                // Nothing before the second quarter; then 7+ on sight and
                // the occasional 6-letter cash-in; dump everything in the
                // last three auctions.
                if t.progress() < 0.25 {
                    return out;
                }
                if t.bag_left <= 3 {
                    return self.final_plays(t);
                }
                while let Some(s) = d.best_word(&r, 7) {
                    r.sub(&s.rack);
                    out.push(s.word.to_string());
                }
                if self.rng.gen_bool(self.mid_cash) {
                    if let Some(s) = d.best_word(&r, 6) {
                        r.sub(&s.rack);
                        out.push(s.word.to_string());
                    }
                }
            }
        }
        out
    }

    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        let d = dictionary();
        let mut s = Solver::with_budget(d, 200_000);
        s.solve(&t.my_rack)
            .word_strings(d)
            .into_iter()
            .map(str::to_string)
            .collect()
    }
}

// ------------------------------------------------ clones from public sources

/// Best disjoint-word value of a rack, with a small node budget (the clones
/// bid on every auction of every simulated match).
fn potential(d: &vickrey_core::Dictionary, rack: &Rack) -> f64 {
    Solver::with_budget(d, 20_000).value(rack) as f64
}

/// Reward of the single best word on the rack, or 0.
fn best_single(d: &vickrey_core::Dictionary, rack: &Rack) -> f64 {
    d.best_word(rack, 1).map(|s| s.reward as f64).unwrap_or(0.0)
}

/// Whole-rack dump with a bounded solver.
fn dump_all(rack: &Rack) -> Vec<String> {
    let d = dictionary();
    let mut s = Solver::with_budget(d, 200_000);
    s.solve(rack)
        .word_strings(d)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Nijello (bananatruck/scrabblebot-evo, live weights `params/nijello-live.json`,
/// gen 180). Live rank ~4 on tournament eve. Bid = `floor(min(value·mult,
/// balance))` with `value = ΔQ + 0.333·Δbest + 3.06·face (− 11.5 for a Q
/// without a U)`, `mult = e^0.202 · (balance/(bagLeft+1)/3)^0.3`, ×(1 +
/// 1.57·(7.4 − bagLeft)/7.4) over the last six auctions, the whole balance at
/// bagLeft 1, a flush at bagLeft 0. Plays a word mid-match only when
/// `reward·(1 + 12/(3+balance)) − 0.88·ΔQ > 25`; only 7+ words in the last
/// six unless nearly broke. Its 450 ms look-ahead over the last twelve
/// auctions is approximated by log-normal jitter (sd 0.25) on the formula.
pub struct NijelloClone {
    label: String,
    rng: SmallRng,
}

impl NijelloClone {
    pub fn new(label: &str, seed: u64) -> Self {
        NijelloClone {
            label: label.to_string(),
            rng: SmallRng::seed_from_u64(seed),
        }
    }
}

impl Agent for NijelloClone {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        let balance = t.my_balance().max(0);
        if t.is_last_auction() || balance == 0 {
            return 0;
        }
        let bag_left = t.bag_left as f64;
        let d = dictionary();
        let rack = t.my_rack;
        let plus = rack.with(letter);
        let dq = potential(d, &plus) - potential(d, &rack);
        let dbest = best_single(d, &plus) - best_single(d, &rack);
        let face = VALUE[letter] as f64;
        let mut value = dq + 0.333 * dbest + 3.06 * face;
        if Some(letter) == vickrey_core::letters::index('Q') {
            let u = vickrey_core::letters::index('U').unwrap();
            if rack.count(u) == 0 {
                value -= 11.5;
            }
        }
        if value <= 0.0 {
            return 0;
        }
        if t.bag_left == 1 {
            return balance;
        }
        let budget = balance as f64 / (bag_left + 1.0);
        let mut mult = (0.202f64).exp() * (budget / 3.0).max(1e-3).powf(0.3);
        if bag_left <= 6.4 {
            mult *= 1.0 + 1.57 * (7.4 - bag_left) / 7.4;
        }
        let mut bid = value * mult;
        if t.bag_left <= 12 {
            let z: f64 = self.rng.sample(rand_distr_normal());
            bid *= (0.25 * z).exp();
        }
        (bid.floor() as i64).clamp(0, balance)
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        if t.is_last_auction() {
            return dump_all(&t.my_rack);
        }
        let d = dictionary();
        let balance = t.my_balance().max(0) as f64;
        let mut r = t.my_rack;
        let mut out = Vec::new();
        let endgame = t.bag_left <= 6;
        loop {
            let min_len = if endgame && balance >= 10.5 { 7 } else { 1 };
            let Some(s) = d.best_word(&r, min_len) else {
                break;
            };
            let mut rest = r;
            rest.sub(&s.rack);
            let dq = potential(d, &r) - potential(d, &rest);
            let gain = s.reward as f64 * (1.0 + 12.0 / (3.0 + balance)) - 0.88 * dq;
            if gain <= 25.0 {
                break;
            }
            out.push(s.word.to_string());
            r = rest;
        }
        out
    }

    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        dump_all(&t.my_rack)
    }
}

/// SookraBot (StephenSook/sookrabot, branch `round3`). Bid = `floor(κ·Δvalue)`,
/// `κ = 3.8·clamp(√r, 0.5, 3)` with `r = (balance/auctionsLeft)/(100/68)`,
/// ×0.55 for the first 28 auctions, ×2 once fewer than 8 tiles remain. Holds
/// every word for the final flush (last three auctions) except a liquidity
/// word when the balance drops under 15.
pub struct SookraClone {
    label: String,
    rng: SmallRng,
}

impl SookraClone {
    pub fn new(label: &str, seed: u64) -> Self {
        SookraClone {
            label: label.to_string(),
            rng: SmallRng::seed_from_u64(seed),
        }
    }
}

impl Agent for SookraClone {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        let balance = t.my_balance().max(0);
        if t.is_last_auction() || balance == 0 {
            return 0;
        }
        let d = dictionary();
        let rack = t.my_rack;
        let dv = potential(d, &rack.with(letter)) - potential(d, &rack);
        if dv <= 0.0 {
            return 0;
        }
        let auctions_left = (t.bag_left + 1) as f64;
        let r = (balance as f64 / auctions_left) / (100.0 / 68.0);
        let mut kappa = 3.8 * r.sqrt().clamp(0.5, 3.0);
        if t.auctions_seen < 28 {
            kappa *= 0.55;
        }
        if t.bag_left < 8 {
            kappa *= 2.0;
        }
        let z: f64 = self.rng.sample(rand_distr_normal());
        let bid = kappa * dv * (0.1 * z).exp();
        (bid.floor() as i64).clamp(0, balance)
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        if t.bag_left <= 2 {
            return dump_all(&t.my_rack);
        }
        if t.my_balance() < 15 {
            let d = dictionary();
            let w = d
                .best_word(&t.my_rack, 5)
                .or_else(|| d.best_word(&t.my_rack, 1));
            return w.map(|s| vec![s.word.to_string()]).unwrap_or_default();
        }
        Vec::new()
    }

    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        dump_all(&t.my_rack)
    }
}

/// Gaucho (JoaquinGiorgis/scrabblebot-gaucho, `params/vickrey.json`). Bid =
/// `1.156·Δpotential + 1.572·face + 3.394 + 0.914·balance/auctionsLeft`.
/// Never plays a word paying under 38 unless nearly broke; otherwise plays
/// when `reward + potential(rest) ≥ 0.934·potential(rack)`; flushes the rack
/// as the last playable auction opens.
pub struct GauchoClone {
    label: String,
}

impl GauchoClone {
    pub fn new(label: &str) -> Self {
        GauchoClone {
            label: label.to_string(),
        }
    }
}

impl Agent for GauchoClone {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        let balance = t.my_balance().max(0);
        if t.is_last_auction() || balance == 0 {
            return 0;
        }
        let d = dictionary();
        let rack = t.my_rack;
        let dv = potential(d, &rack.with(letter)) - potential(d, &rack);
        let auctions_left = (t.bag_left + 1) as f64;
        let bid = 1.156 * dv
            + 1.572 * VALUE[letter] as f64
            + 3.394
            + 0.914 * balance as f64 / auctions_left;
        (bid.floor() as i64).clamp(0, balance)
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        if t.bag_left <= 1 {
            return dump_all(&t.my_rack);
        }
        let d = dictionary();
        let balance = t.my_balance() as f64;
        let mut r = t.my_rack;
        let mut out = Vec::new();
        let base = potential(d, &r);
        while let Some(s) = d.best_word(&r, 1) {
            if (s.reward as f64) < 38.0 && balance >= 9.9 {
                break;
            }
            let mut rest = r;
            rest.sub(&s.rack);
            if s.reward as f64 + potential(d, &rest) < 0.934 * base {
                break;
            }
            out.push(s.word.to_string());
            r = rest;
        }
        out
    }

    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        dump_all(&t.my_rack)
    }
}

// ------------------------------------------------------------ our brain

pub struct Vickrey {
    label: String,
    pub brain: Brain,
    /// When set, one line per decision is appended (for `trace`).
    pub trace: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
}

impl Vickrey {
    pub fn new(label: &str, params: Params, field: FieldModel, seed: u64) -> Self {
        Vickrey {
            label: label.to_string(),
            brain: Brain::new(params, field, seed),
            trace: None,
        }
    }
}

impl Agent for Vickrey {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn start(&mut self, _t: &Tracker) {
        self.brain.start_match();
    }

    fn bid(&mut self, t: &Tracker, letter: Letter) -> i64 {
        let d = self.brain.bid(t, letter);
        if let Some(tr) = &self.trace {
            tr.lock().unwrap().push(format!(
                "#{:<3} bag {:>2} {} rack {:<14} bal {:>4} maxopp {:>4} heat {:.2} | V {:>6.2} λ {:.3} bid {:>4} p {:.2}",
                t.auctions_seen,
                t.bag_left,
                vickrey_core::char_of(letter),
                t.my_rack.to_string(),
                t.my_balance(),
                t.max_opponent_balance(),
                self.brain.market.heat,
                d.value,
                d.lambda,
                d.amount,
                d.p_win
            ));
        }
        d.amount
    }

    fn observe(&mut self, t: &Tracker, letter: Letter, winner: Option<u64>, paid: i64) {
        if let Some(tr) = &self.trace {
            let who = match winner {
                Some(w) if w == t.my_id => "WON".to_string(),
                Some(w) => format!("lost to {w}"),
                None => "no bids".to_string(),
            };
            tr.lock()
                .unwrap()
                .push(format!("      -> {} paid {}", who, paid));
        }
        self.brain.observe_result(t, letter, winner, paid);
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        let w: Vec<String> = self
            .brain
            .plays(t)
            .into_iter()
            .map(str::to_string)
            .collect();
        if let (Some(tr), false) = (&self.trace, w.is_empty()) {
            tr.lock().unwrap().push(format!("      PLAY {:?}", w));
        }
        w
    }

    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        self.brain
            .final_plays(&t.my_rack)
            .into_iter()
            .map(str::to_string)
            .collect()
    }
}
