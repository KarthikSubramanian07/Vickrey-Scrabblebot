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

/// Which fitted live family a [`LiveClone`] imitates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneKind {
    /// Doppio / Nijello / Heidi / benx: hoard early, ramp bids with match
    /// progress and balance, spend 67–79 coins in the last quarter, dump at
    /// the end. Fitted on 250 live matches: `−5 + 2.8·face + 18·progress +
    /// 13·balance/100`, residual sd ≈ 5.
    Ramp,
    /// guesser / postturing / atlas / Chinese Dante: `9.5 + 3·face`, sd 3,
    /// regardless of time or balance; plays 5+ letter words on sight.
    Flat,
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
                -5.0 + 2.8 * face + 18.0 * t.progress() + 13.0 * bal / 100.0 + 5.0 * z
            }
            CloneKind::Flat => 9.5 + 3.0 * face + 3.0 * z,
        };
        (b.round() as i64).clamp(0, t.my_balance().max(0))
    }

    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        let d = dictionary();
        let mut r = t.my_rack;
        let mut out = Vec::new();
        match self.kind {
            CloneKind::Flat => {
                while let Some(s) = d.best_word(&r, 5) {
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
