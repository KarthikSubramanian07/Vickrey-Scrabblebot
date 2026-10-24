//! The decision maker: what a letter is worth, what to bid, what to play.
//!
//! **Valuation.** A rack's worth is what its final dump would score: the
//! best set of disjoint words (`vickrey_core::solver`). Mid-match we also
//! expect to win more letters, so we sample futures — the letters we might
//! still buy, drawn from the unseen pool — and a letter's marginal value is
//! `E[T(R + L + F)] − E[T(R + F)]` over shared samples (common random
//! numbers keep the difference low-variance).
//!
//! **Bidding.** Spending never lowers score; leftover coins are worthless.
//! So the only question is pacing: convert points to coins at a rate `λ`
//! (points per coin) chosen so the expected spend over the remaining
//! auctions matches the budget, then bid `V / λ`. Under Vickrey that
//! truthful bid is optimal; under first-price we pick the bid maximising
//! expected surplus against the opposing-bid distribution.
//!
//! **Playing.** Words only score if played before the match ends, and coins
//! from a play buy more letters. We play a word mid-match when its reward
//! plus that liquidity beats the expected value of holding its letters.
//! When the last auction opens we dump the exact best partition.

use crate::market::{BidDist, FieldModel, Market};
use crate::params::Params;
use crate::tracker::{AuctionKind, Tracker};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use vickrey_core::letters::VALUE;
use vickrey_core::{dictionary, Dictionary, Letter, Rack, Solver, ALPHABET};

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct BidDecision {
    pub amount: i64,
    /// Marginal value in points.
    pub value: f64,
    /// Points per coin used to convert.
    pub lambda: f64,
    /// Modelled chance of winning at `amount`.
    pub p_win: f64,
}

/// A run of future auctions: `(count, letter mix)`.
type Segment = (f64, Vec<(f64, f64, BidDist)>);

pub struct Brain {
    /// Active parameters (switched by table size, see `sync_profile`).
    pub params: Params,
    /// Parameters for tables of three or more.
    pub multi: Params,
    /// Optional parameters for heads-up tables (the tournament final).
    pub duel: Option<Params>,
    pub market: Market,
    dict: &'static Dictionary,
    rng: SmallRng,
    /// Marginal value per letter for the current rack (pacing input).
    table: [f64; ALPHABET],
    table_rack: Option<Rack>,
    table_at: u32,
    pub last_lambda: f64,
    /// Our bid on the open auction, to read censored prices correctly.
    last_bid: i64,
}

impl Brain {
    pub fn new(params: Params, field: FieldModel, seed: u64) -> Self {
        // Pay one-time setup (dictionary index, letter prior) up front, not mid-auction.
        let _ = letter_prior();
        Brain {
            params: params.clone(),
            multi: params,
            duel: None,
            market: Market::new(field),
            dict: dictionary(),
            rng: SmallRng::seed_from_u64(seed),
            table: [0.0; ALPHABET],
            table_rack: None,
            table_at: 0,
            last_lambda: 0.3,
            last_bid: 0,
        }
    }

    /// Use a dedicated parameter set at heads-up tables.
    pub fn with_duel(mut self, duel: Option<Params>) -> Self {
        self.duel = duel;
        self
    }

    /// Pick the parameter profile for this table.
    pub fn sync_profile(&mut self, t: &Tracker) {
        let want = match (&self.duel, t.n_players() <= 2) {
            (Some(d), true) => d,
            _ => &self.multi,
        };
        if &self.params != want {
            self.params = want.clone();
        }
    }

    /// Latest per-letter marginal values used for pacing.
    pub fn value_table(&self) -> [f64; ALPHABET] {
        self.table
    }

    /// Forget per-match state.
    pub fn start_match(&mut self) {
        let field = self.market.field.clone();
        self.market = Market::new(field);
        self.table_rack = None;
        self.last_lambda = 0.3;
    }

    // ------------------------------------------------------------ valuation

    fn terminal(&self, rack: &Rack) -> f64 {
        if self.params.solve_nodes == 0 {
            vickrey_core::solver::greedy(self.dict, rack).score as f64
        } else {
            Solver::with_budget(self.dict, self.params.solve_nodes)
                .solve(rack)
                .score as f64
        }
    }

    /// Exact value of a small rack, for the endgame where no future letters
    /// are sampled and greedy can misprice one extra letter.
    fn terminal_exact(&self, rack: &Rack) -> f64 {
        Solver::with_budget(self.dict, 20_000).solve(rack).score as f64
    }

    /// Future auctions whose letters we could still play (the last never can).
    fn usable_future(t: &Tracker) -> u32 {
        t.bag_left.saturating_sub(1)
    }

    /// How many more letters we expect to win after the current auction.
    fn expected_wins(&self, t: &Tracker) -> f64 {
        let n = Self::usable_future(t) as f64;
        if n <= 0.0 {
            return 0.0;
        }
        let balance = t.my_balance().max(0) as f64;
        let budget = balance * (1.0 + self.params.income_factor * (1.0 - t.progress()));
        let opp = t.n_players().saturating_sub(1).max(1);
        let unseen = t.unseen();
        let total = unseen.len().max(1) as f64;
        let mut avg_price = 0.0;
        for l in 0..ALPHABET {
            let w = unseen.0[l] as f64 / total;
            if w > 0.0 {
                avg_price += w * self
                    .market
                    .dist(l, t.progress(), opp, t.max_opponent_balance())
                    .median;
            }
        }
        let fair_share = n / t.n_players().max(1) as f64;
        let affordable = budget / avg_price.max(1.0);
        (affordable.min(n).max(fair_share * 0.25) * self.params.win_share_scale).min(n)
    }

    fn sample_futures(&mut self, unseen: &Rack, k: usize, n: usize) -> Vec<Rack> {
        let pool: Vec<Letter> = unseen.letters().collect();
        let k = k.min(pool.len());
        let mut out = Vec::with_capacity(n);
        let mut buf = pool.clone();
        for _ in 0..n {
            let mut f = Rack::EMPTY;
            for i in 0..k {
                let j = self.rng.gen_range(i..buf.len());
                buf.swap(i, j);
                f.add(buf[i]);
            }
            out.push(f);
        }
        out
    }

    fn future_size(&self, t: &Tracker) -> usize {
        let k = self.expected_wins(t) * self.params.choice_mult;
        (k.round() as usize)
            .min(self.params.max_future)
            .min(Self::usable_future(t) as usize)
    }

    /// Marginal values of each letter in `letters` for rack `rack`, and the
    /// base expectation, over `samples` shared futures.
    pub fn marginal_values(
        &mut self,
        t: &Tracker,
        rack: &Rack,
        letters: &[Letter],
        samples: usize,
    ) -> (f64, Vec<f64>) {
        self.marginal_values_capped(t, rack, letters, samples, u64::MAX)
    }

    /// Like `marginal_values`, but stops sampling once `work` units are
    /// spent. A unit is one letter² of terminal evaluation (greedy cost grows
    /// roughly with the square of the rack), about 1.7 µs, so the budget is
    /// deterministic yet tracks wall-clock time.
    pub fn marginal_values_capped(
        &mut self,
        t: &Tracker,
        rack: &Rack,
        letters: &[Letter],
        samples: usize,
        work: u64,
    ) -> (f64, Vec<f64>) {
        // The open auction's letter is already revealed, so it is not in `unseen`.
        let unseen = t.unseen();
        let k = self.future_size(t);
        let futures = if k == 0 {
            vec![Rack::EMPTY]
        } else {
            self.sample_futures(&unseen, k, samples.max(1))
        };
        // Endgame: nothing left to sample, so value the rack exactly.
        let exact = k == 0
            && self.params.exact_terminal_max > 0
            && rack.len() < self.params.exact_terminal_max;
        let eval = |me: &Brain, r: &Rack| {
            if exact {
                me.terminal_exact(r)
            } else {
                me.terminal(r)
            }
        };
        let mut base = 0.0;
        let mut with = vec![0.0; letters.len()];
        let mut used = 0u64;
        let mut n = 0usize;
        for f in &futures {
            let mut r = *rack;
            r.plus(f);
            base += eval(self, &r);
            for (j, &l) in letters.iter().enumerate() {
                with[j] += eval(self, &r.with(l));
            }
            n += 1;
            let len = r.len() as u64 + 1;
            used += len * len * (letters.len() as u64 + 1);
            if n >= 3 && used >= work {
                break;
            }
        }
        let n = n.max(1) as f64;
        (
            base / n,
            with.into_iter()
                .map(|w| ((w - base) / n).max(0.0))
                .collect(),
        )
    }

    /// Value of letter `l` to the strongest rival we can model (for denial).
    fn rival_value(&mut self, t: &Tracker, l: Letter) -> f64 {
        let mut best = 0.0f64;
        let rivals: Vec<Rack> = t.opponents().map(|p| p.known_rack()).collect();
        for r in rivals {
            let (_, v) = self.marginal_values_capped(
                t,
                &r,
                &[l],
                self.params.table_samples.max(4),
                self.params.work_budget / 4,
            );
            best = best.max(v[0]);
        }
        best
    }

    fn refresh_table(&mut self, t: &Tracker) {
        let stale = self.table_rack != Some(t.my_rack) || t.auctions_seen >= self.table_at + 4;
        if !stale {
            return;
        }
        let all: Vec<Letter> = (0..ALPHABET).collect();
        let (_, v) = self.marginal_values_capped(
            t,
            &t.my_rack.clone(),
            &all,
            self.params.table_samples,
            self.params.work_budget,
        );
        let prior = letter_prior();
        let w = self.params.table_shrink.clamp(0.0, 1.0);
        for (l, x) in v.into_iter().enumerate() {
            self.table[l] = (1.0 - w) * x + w * prior[l];
        }
        self.table_rack = Some(t.my_rack);
        self.table_at = t.auctions_seen;
    }

    // ------------------------------------------------------------- bidding

    fn dist(&self, t: &Tracker, l: Letter) -> BidDist {
        self.dist_at(t, l, t.progress())
    }

    fn dist_at(&self, t: &Tracker, l: Letter, progress: f64) -> BidDist {
        self.market.dist(
            l,
            progress,
            t.n_players().saturating_sub(1),
            t.max_opponent_balance(),
        )
    }

    /// (weight, value, opposing-bid distribution) for every letter that can
    /// still come up, priced as of match progress `progress`.
    fn letter_mix_at(&self, t: &Tracker, progress: f64) -> Vec<(f64, f64, BidDist)> {
        let unseen = t.unseen();
        let total = unseen.len().max(1) as f64;
        (0..ALPHABET)
            .filter(|&l| unseen.0[l] > 0)
            .map(|l| {
                (
                    unseen.0[l] as f64 / total,
                    self.table[l],
                    self.dist_at(t, l, progress),
                )
            })
            .collect()
    }

    /// The usable auctions left, as `(count, letter mix)` segments. With
    /// phase-aware pacing each segment is priced at its own point in the
    /// match, so the hot opening and the cheap closing are both visible to
    /// the budget; otherwise everything is priced as of now.
    fn segments(&self, t: &Tracker) -> Vec<Segment> {
        let n = Self::usable_now(t);
        if !self.params.phase_aware_pacing || n <= 1.0 {
            return vec![(n, self.letter_mix_at(t, t.progress()))];
        }
        let total = t.total_auctions.max(1) as f64;
        let seen = t.auctions_seen as f64;
        let k = (n as usize).clamp(1, 6);
        let per = n / k as f64;
        (0..k)
            .map(|b| {
                let mid = seen + per * (b as f64 + 0.5);
                (per, self.letter_mix_at(t, (mid / total).clamp(0.0, 1.0)))
            })
            .collect()
    }

    /// Coins we plan to spend from here on if our balance were `balance`.
    fn budget_for(&self, t: &Tracker, balance: f64) -> f64 {
        let p = &self.params;
        balance * (1.0 + p.income_factor * (1.0 - t.progress())) * p.spend_target
    }

    /// λ that makes expected spend over `n` auctions match `budget`, with
    /// every bid capped at `cap` coins.
    fn solve_lambda(&self, segs: &[Segment], cap: f64, budget: f64) -> f64 {
        let p = &self.params;
        let spend = |lam: f64| -> f64 {
            segs.iter()
                .map(|(n, mix)| {
                    mix.iter()
                        .map(|(w, v, d)| w * d.expected_pay_vickrey((v / lam).min(cap)))
                        .sum::<f64>()
                        * n
                })
                .sum::<f64>()
        };
        let (lo, hi) = (p.lambda_min, p.lambda_max);
        if spend(lo) <= budget {
            return lo;
        }
        if spend(hi) >= budget {
            return hi;
        }
        // spend() falls as λ rises; bisect in log space.
        let (mut a, mut b) = (lo.ln(), hi.ln());
        for _ in 0..30 {
            let m = 0.5 * (a + b);
            if spend(m.exp()) > budget {
                a = m;
            } else {
                b = m;
            }
        }
        (0.5 * (a + b)).exp()
    }

    /// Expected points bought over `n` auctions at shadow price `lam` with bids capped at `cap`.
    fn expected_points(segs: &[Segment], lam: f64, cap: f64) -> f64 {
        segs.iter()
            .map(|(n, mix)| {
                mix.iter()
                    .map(|(w, v, d)| w * v * d.p_win((v / lam).min(cap)))
                    .sum::<f64>()
                    * n
            })
            .sum()
    }

    /// Auctions still to come whose letters we can use, including the open one.
    fn usable_now(t: &Tracker) -> f64 {
        (Self::usable_future(t) + 1) as f64
    }

    /// Solve for λ so expected spend over the usable auctions left matches budget.
    fn pace(&mut self, t: &Tracker) -> f64 {
        let balance = t.my_balance().max(0) as f64;
        let segs = self.segments(t);
        self.solve_lambda(&segs, balance, self.budget_for(t, balance))
    }

    /// Marginal points from one more coin right now (μ). Unlike the pacing
    /// λ — which collapses to its floor when we are broke, because capped
    /// bids spend little — μ is high exactly when the balance cap binds.
    /// Finite difference of expected points bought with `B` vs `B + Δ` coins.
    pub fn money_value(&mut self, t: &Tracker) -> f64 {
        if t.is_last_auction() || Self::usable_future(t) == 0 {
            return 0.0;
        }
        self.refresh_table(t);
        let b0 = t.my_balance().max(0) as f64;
        let delta = (0.25 * b0).max(10.0);
        let b1 = b0 + delta;
        let segs = self.segments(t);
        let l0 = self.solve_lambda(&segs, b0, self.budget_for(t, b0));
        let l1 = self.solve_lambda(&segs, b1, self.budget_for(t, b1));
        let gain = Self::expected_points(&segs, l1, b1) - Self::expected_points(&segs, l0, b0);
        (gain / delta).clamp(0.0, 3.0)
    }

    /// Decide our bid on the open auction for `letter`.
    pub fn bid(&mut self, t: &Tracker, letter: Letter) -> BidDecision {
        let d = self.bid_inner(t, letter);
        self.last_bid = d.amount;
        d
    }

    fn bid_inner(&mut self, t: &Tracker, letter: Letter) -> BidDecision {
        self.sync_profile(t);
        let balance = t.my_balance().max(0);
        if t.is_last_auction() || balance <= 0 {
            return BidDecision::default();
        }
        self.refresh_table(t);
        let (_, v) = self.marginal_values_capped(
            t,
            &t.my_rack.clone(),
            &[letter],
            self.params.samples,
            self.params.work_budget,
        );
        let mut value = v[0];
        self.table[letter] = value;
        if self.params.deny_weight > 0.0 && t.n_players() <= self.params.deny_max_players {
            value += self.params.deny_weight * self.rival_value(t, letter);
        }
        let lambda = self.pace(t);
        self.last_lambda = lambda;
        if value < self.params.min_value {
            let amount = if value >= self.params.cheap_grab_value.min(self.params.min_value)
                && value > 0.0
            {
                1
            } else {
                0
            };
            let d = self.dist(t, letter);
            return BidDecision {
                amount,
                value,
                lambda,
                p_win: d.p_win(amount as f64),
            };
        }
        let d = self.dist(t, letter);
        let mut truthful = (value / lambda).min(balance as f64);
        // Endgame: with few usable auctions left, leftover coins are worth
        // nothing, so a letter worth real points deserves everything we have.
        // Under Vickrey we only pay the runner-up, so this costs nothing extra
        // when we would have won anyway.
        let left = Self::usable_future(t) + 1;
        if self.params.endgame_allin_auctions > 0
            && left <= self.params.endgame_allin_auctions
            && value >= self.params.endgame_allin_min_value
        {
            truthful = balance as f64;
        }
        // Prices run ~35% hot in the opening quarter; optionally bid under value there.
        if self.params.opening_shade > 0.0 && t.progress() < 0.25 {
            truthful *= 1.0 - self.params.opening_shade.min(0.9);
        }
        let mut amount = match t.kind {
            AuctionKind::Vickrey => truthful.floor() as i64,
            AuctionKind::FirstPrice if self.params.first_price_shade => {
                // argmax_b (v/λ − b) · P(win | b)
                let mut best = (0.0, 0i64);
                for b in 1..=(truthful.floor() as i64) {
                    let s = (truthful - b as f64) * d.p_win(b as f64);
                    if s > best.0 {
                        best = (s, b);
                    }
                }
                best.1
            }
            AuctionKind::FirstPrice => truthful.floor() as i64,
        };
        if amount < 1 && value >= self.params.cheap_grab_value {
            amount = 1;
        }
        // Volume floor: on ordinary letters that fit, bid at least a share of
        // the market price, so we collect the letters nobody else is
        // fighting over (Vickrey: we pay the rivals' bid, not ours). The live
        // leader wins 4.5 opening letters at 11.7 this way; we paid 15.6.
        if self.params.volume_floor > 0.0
            && VALUE[letter] < 5
            && value >= self.params.volume_min_value
            && !t.is_last_auction()
        {
            let floor = (self.params.volume_floor * d.median).floor() as i64;
            amount = amount.max(floor);
        }
        // Premium letters (J, K, Q, X, Z) need specific words to pay off; the
        // live league's best bot refuses to pay much over market for them.
        if self.params.premium_cap_mult > 0.0 && VALUE[letter] >= 5 {
            let cap = (self.params.premium_cap_mult * d.median).ceil() as i64;
            amount = amount.min(cap.max(1));
        }
        let amount = amount.clamp(0, balance);
        BidDecision {
            amount,
            value,
            lambda,
            p_win: d.p_win(amount as f64),
        }
    }

    /// Update the price model with a closed auction.
    pub fn observe_result(&mut self, t: &Tracker, letter: Letter, winner: Option<u64>, paid: i64) {
        let we_won = winner == Some(t.my_id);
        if winner.is_some() {
            self.market.observe(
                letter,
                t.progress(),
                paid,
                we_won,
                t.n_players().saturating_sub(1),
                self.last_bid,
            );
        }
    }

    // ------------------------------------------------------------- playing

    /// Words to play now, mid-match. Empty when holding is better.
    pub fn plays(&mut self, t: &Tracker) -> Vec<&'static str> {
        self.sync_profile(t);
        if t.is_last_auction() {
            return self.final_plays(&t.my_rack);
        }
        // In the closing stretch letters are cheap and coins from the final
        // dump arrive too late to spend, so shorter words may be cashed early.
        let late = self.params.late_window > 0 && t.bag_left <= self.params.late_window;
        // Broke with auctions still to come: every auction we sit out is a
        // letter a rival gets cheaply. Live matches showed 20–40-auction
        // droughts while a playable 5–6 letter word sat in the rack.
        let starved = t.my_balance() < self.params.starve_coins && Self::usable_future(t) >= 3;
        let mut min_len = self.params.min_play_len;
        if late {
            min_len = min_len.min(self.params.late_min_play_len);
        }
        if starved {
            min_len = min_len.min(self.params.starve_min_play_len);
        }
        if self.dict.candidates(&t.my_rack, min_len).is_empty() {
            return Vec::new();
        }
        let mu = if self.params.mu_liquidity || late || starved {
            self.money_value(t)
        } else {
            self.last_lambda
        };
        let p = self.params.clone();
        let mut rack = t.my_rack;
        let mut out = Vec::new();
        let k = self.future_size(t);
        let futures = if k == 0 {
            vec![Rack::EMPTY]
        } else {
            self.sample_futures(&t.unseen(), k, p.samples.max(8) / 2)
        };
        let eval = |me: &Brain, r: &Rack| -> f64 {
            futures
                .iter()
                .map(|f| {
                    let mut x = *r;
                    x.plus(f);
                    me.terminal(&x)
                })
                .sum::<f64>()
                / futures.len() as f64
        };
        for _ in 0..3 {
            let mut cands: Vec<u32> = self.dict.candidates(&rack, min_len);
            if cands.is_empty() {
                break;
            }
            cands.sort_unstable_by_key(|&i| std::cmp::Reverse(self.dict.sig(i).reward));
            cands.truncate(p.play_candidates);
            let hold = eval(self, &rack);
            let mut best: Option<(f64, u32)> = None;
            for &i in &cands {
                let s = self.dict.sig(i);
                let mut rest = rack;
                rest.sub(&s.rack);
                let gain = s.reward as f64 * (1.0 + p.liquidity * mu) + eval(self, &rest) - hold;
                if gain > p.play_margin && best.is_none_or(|(g, _)| gain > g) {
                    best = Some((gain, i));
                }
            }
            let Some((_, i)) = best else { break };
            let s = self.dict.sig(i);
            rack.sub(&s.rack);
            out.push(s.word);
        }
        out
    }

    /// Everything we can play, for the last auction.
    pub fn final_plays(&self, rack: &Rack) -> Vec<&'static str> {
        let mut s = Solver::with_deadline(self.dict, self.params.final_solve_ms);
        s.solve(rack).word_strings(self.dict)
    }

    /// Face value per letter, for logging.
    pub fn face(l: Letter) -> u32 {
        VALUE[l]
    }
}

/// Average marginal value of each letter over random mid-match racks
/// (12 held + 8 to come), computed once per process. Used to shrink the
/// noisy per-auction value table.
pub fn letter_prior() -> &'static [f64; ALPHABET] {
    static PRIOR: std::sync::OnceLock<[f64; ALPHABET]> = std::sync::OnceLock::new();
    PRIOR.get_or_init(|| {
        let d = dictionary();
        let mut rng = SmallRng::seed_from_u64(0x5eed);
        let pool: Vec<Letter> = (0..ALPHABET)
            .flat_map(|l| std::iter::repeat_n(l, vickrey_core::BAG[l] as usize))
            .collect();
        let mut sum = [0.0; ALPHABET];
        let n = 160;
        for _ in 0..n {
            let mut buf = pool.clone();
            let mut r = Rack::EMPTY;
            for i in 0..20 {
                let j = rng.gen_range(i..buf.len());
                buf.swap(i, j);
                r.add(buf[i]);
            }
            let base = vickrey_core::solver::greedy(d, &r).score as f64;
            for (l, slot) in sum.iter_mut().enumerate() {
                *slot += (vickrey_core::solver::greedy(d, &r.with(l)).score as f64 - base).max(0.0);
            }
        }
        sum.map(|x| x / n as f64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vickrey_core::index;

    fn tracker_with(rack: &str, bag_left: u32) -> Tracker {
        let mut t = Tracker::new(1, 1, AuctionKind::Vickrey, &[1, 2, 3, 4, 5, 6], 5);
        t.set_my_rack(Rack::from_word(rack));
        t.on_auction_open(index('E').unwrap(), bag_left);
        t
    }

    #[test]
    fn last_auction_is_worthless() {
        let mut b = Brain::new(Params::default(), FieldModel::default(), 1);
        let t = tracker_with("QUIZAS", 0);
        assert_eq!(b.bid(&t, index('E').unwrap()).amount, 0);
    }

    #[test]
    fn completing_letter_is_valued() {
        let mut b = Brain::new(Params::default(), FieldModel::default(), 1);
        // CAZIQU + E = CAZIQUE (81 points) right before the end.
        let t = tracker_with("CAZIQU", 1);
        let d = b.bid(&t, index('E').unwrap());
        assert!(d.value > 40.0, "{d:?}");
        assert!(d.amount >= 50, "{d:?}");
    }

    #[test]
    fn final_dump_plays_everything_useful() {
        let b = Brain::new(Params::default(), FieldModel::default(), 1);
        let words = b.final_plays(&Rack::from_word("CAZIQUEXI"));
        assert!(words.iter().any(|w| w.len() == 7));
    }

    #[test]
    fn money_is_worth_more_when_broke() {
        let mut b = Brain::new(Params::default(), FieldModel::default(), 5);
        let mut t = tracker_with("ACDESUX", 20);
        t.on_participant(1, 8, 50);
        let broke = b.money_value(&t);
        t.on_participant(1, 200, 50);
        let rich = b.money_value(&t);
        assert!(broke > rich, "broke {broke} rich {rich}");
        assert!(broke > 0.05);
    }

    #[test]
    fn cashes_short_word_when_starved() {
        let mut b = Brain::new(Params::default(), FieldModel::default(), 9);
        // JIVER (5 letters, 30 points) with 38 auctions left and 7 coins.
        let mut t = tracker_with("EIJRV", 38);
        t.on_participant(1, 7, 48);
        assert_eq!(b.plays(&t), vec!["JIVER"]);
        // Rich: keep building towards a 7-letter word.
        t.on_participant(1, 90, 48);
        assert!(b.plays(&t).is_empty());
    }

    #[test]
    fn premium_cap_limits_premium_bids_only() {
        let capped = Params {
            premium_cap_mult: 0.5,
            ..Params::default()
        };
        let mut b = Brain::new(capped, FieldModel::default(), 4);
        // Z completes a 7-letter word: valuable, but capped at half the market.
        let t = tracker_with("ABLEIT", 20);
        let z = b.bid(&t, index('Z').unwrap());
        let median = b
            .market
            .dist(index('Z').unwrap(), t.progress(), 5, 100)
            .median;
        assert!(
            (z.amount as f64) <= (0.5 * median).ceil(),
            "{z:?} median {median}"
        );
        // Common letters are untouched.
        let mut free = Brain::new(Params::default(), FieldModel::default(), 4);
        let mut capped2 = Brain::new(
            Params {
                premium_cap_mult: 0.5,
                ..Params::default()
            },
            FieldModel::default(),
            4,
        );
        let e = index('E').unwrap();
        assert_eq!(free.bid(&t, e).amount, capped2.bid(&t, e).amount);
    }

    #[test]
    fn opening_shade_only_applies_early() {
        let shaded = Params {
            opening_shade: 0.3,
            ..Params::default()
        };
        let e = index('E').unwrap();
        // Early: shaded bid is lower.
        let early = tracker_with("ABLEIT", 66);
        let (mut a, mut b) = (
            Brain::new(Params::default(), FieldModel::default(), 9),
            Brain::new(shaded.clone(), FieldModel::default(), 9),
        );
        let (plain, shade) = (a.bid(&early, e).amount, b.bid(&early, e).amount);
        assert!(shade < plain || plain == 0, "plain {plain} shaded {shade}");
        // Late (progress > 25%): identical.
        let mut late = tracker_with("ABLEIT", 20);
        for _ in 0..40 {
            late.on_auction_open(e, 20);
        }
        let (mut a, mut b) = (
            Brain::new(Params::default(), FieldModel::default(), 9),
            Brain::new(shaded, FieldModel::default(), 9),
        );
        assert_eq!(a.bid(&late, e).amount, b.bid(&late, e).amount);
    }

    #[test]
    fn phase_aware_pacing_gives_a_sane_lambda() {
        let p = Params {
            phase_aware_pacing: true,
            ..Params::default()
        };
        let mut b = Brain::new(p.clone(), FieldModel::default(), 2);
        let t = tracker_with("ABLEIT", 60);
        let d = b.bid(&t, index('S').unwrap());
        assert!(
            d.lambda >= p.lambda_min && d.lambda <= p.lambda_max,
            "{d:?}"
        );
        assert!(d.amount >= 0 && d.amount <= 100);
    }

    #[test]
    fn exact_terminal_sees_what_greedy_misses() {
        // Greedy grabs the single best word first; the exact solver finds
        // the better two-word split. Exact valuation must never be lower.
        let exact = Brain::new(
            Params {
                exact_terminal_max: 16,
                ..Params::default()
            },
            FieldModel::default(),
            1,
        );
        let greedy = Brain::new(Params::default(), FieldModel::default(), 1);
        let mut rng = SmallRng::seed_from_u64(3);
        let pool: Vec<Letter> = (0..ALPHABET)
            .flat_map(|l| std::iter::repeat_n(l, vickrey_core::BAG[l] as usize))
            .collect();
        let mut strictly_better = 0;
        for _ in 0..60 {
            let mut r = Rack::EMPTY;
            for _ in 0..12 {
                r.add(pool[rng.gen_range(0..pool.len())]);
            }
            let (e, g) = (exact.terminal_exact(&r), greedy.terminal(&r));
            assert!(e >= g, "exact {e} < greedy {g} for {r}");
            strictly_better += (e > g) as u32;
        }
        assert!(strictly_better > 0);
    }

    #[test]
    fn volume_floor_lifts_low_bids_on_fitting_common_letters() {
        let t = tracker_with("ABLEIT", 40);
        let s = index('S').unwrap();
        let mut plain = Brain::new(Params::default(), FieldModel::default(), 6);
        let mut floored = Brain::new(
            Params {
                volume_floor: 0.9,
                volume_min_value: 0.0,
                ..Params::default()
            },
            FieldModel::default(),
            6,
        );
        let (p, f) = (plain.bid(&t, s), floored.bid(&t, s));
        let median = floored.market.dist(s, t.progress(), 5, 100).median;
        assert!(
            f.amount >= (0.9 * median).floor() as i64,
            "{f:?} median {median}"
        );
        assert!(f.amount >= p.amount);
    }

    #[test]
    fn endgame_allin_bids_the_balance_on_valuable_letters() {
        let p = Params {
            endgame_allin_auctions: 6,
            endgame_allin_min_value: 3.0,
            ..Params::default()
        };
        let e = index('E').unwrap();
        // 4 usable auctions left, E completes CAZIQUE: go all in.
        let mut t = tracker_with("CAZIQU", 4);
        t.on_participant(1, 37, 0);
        let mut b = Brain::new(p.clone(), FieldModel::default(), 8);
        assert_eq!(b.bid(&t, e).amount, 37);
        // Same rack with 40 left: normal pacing, well under the balance.
        let mut t2 = tracker_with("CAZIQU", 40);
        t2.on_participant(1, 100, 0);
        let mut b2 = Brain::new(p, FieldModel::default(), 8);
        assert!(b2.bid(&t2, e).amount < 100);
    }

    #[test]
    fn bids_never_exceed_balance() {
        let mut b = Brain::new(Params::default(), FieldModel::default(), 3);
        let mut t = tracker_with("CAZIQU", 1);
        t.on_participant(1, 7, 0);
        assert!(b.bid(&t, index('E').unwrap()).amount <= 7);
    }
}
