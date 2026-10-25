//! The autopilot daemon behind `vickrey run`.
//!
//! **Threads.** The SDK's `run_threaded()` thread applies each server
//! transaction to the client cache and then fires row callbacks. Ours only
//! copy a few fields into an `mpsc` channel. One worker (the main thread)
//! owns every per-match `Tracker`/`Brain`, consumes events in order, reads
//! the cache when it needs the latest truth, and calls reducers through the
//! same `DbConnection` (a reducer call just enqueues a message that wakes the
//! SDK thread, so it is safe and immediate from here).
//!
//! **Ordering.** Row callbacks fire per table in alphabetical order, so for
//! the transaction that closes auction *k* and opens *k+1* we see the new
//! `auction` row before the `auction_result`, `match_participant` and
//! `my_rack` changes. Before deciding on an open auction the worker
//! therefore pulls the previous result straight from the cache (primary-key
//! lookup) and refreshes balances and our rack, which the cache already
//! holds in their post-transaction state. Results, words and opens are
//! de-duplicated by their monotonically increasing ids.
//!
//! **Sequencing within an auction** mirrors the simulator: mid-match words
//! go out first, then the bid, priced with the post-play rack and balance
//! (the server runs one client's reducer calls in order, so the coins land
//! before the bid is checked). When `bag_total == 0` the open auction is the
//! last: we dump the final partition and do not bid.

use crate::config::Common;
use crate::conn::flatten_outcome;
use crate::helpers::{
    derive_deal, feed_tracker, letter_of, now_ms, order_history, percentile, place_of,
    rack_from_rows, rack_minus, HistItem,
};
use crate::module_bindings::*;
use crate::telemetry::{
    BidRec, Heartbeat, MatchLine, PlayerRec, SummaryLine, Telemetry, SCHEMA_VERSION,
};
use anyhow::{bail, Result};
use spacetimedb_sdk::{
    DbContext, Event, Identity, SubscriptionHandle as _, Table, TableWithPrimaryKey,
};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use vickrey_brain::{AuctionKind, Brain, Tracker};
use vickrey_core::{word_reward, Rack};

/// Whom to impersonate in spectator mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shadow {
    Bot(u64),
    /// Every running match, shadowing its highest-rated real bot.
    All,
}

impl std::str::FromStr for Shadow {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        if s.eq_ignore_ascii_case("all") {
            return Ok(Shadow::All);
        }
        s.parse::<u64>()
            .map(Shadow::Bot)
            .map_err(|_| format!("expected a bot id or 'all', got '{s}'"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    Spectate(Shadow),
}

impl Mode {
    fn is_live(&self) -> bool {
        matches!(self, Mode::Live)
    }
    fn name(&self) -> &'static str {
        if self.is_live() {
            "live"
        } else {
            "spectate"
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunOpts {
    pub common: Common,
    pub mode: Mode,
    pub no_lobby: bool,
    pub no_tournaments: bool,
    pub duration: Option<Duration>,
    pub max_reconnect_failures: u32,
    pub slow_ms: f64,
    /// Use reduced sampling when fewer than this many ms remain at decision start.
    pub fast_below_ms: i64,
}

// ------------------------------------------------------------------ events

enum Ev {
    Connected(Identity),
    ConnectFailed(String),
    Disconnected(Option<String>),
    Phase1,
    Ready,
    FloorApplied,
    SubError(String),
    AuctionOpen {
        auction_id: u64,
        match_id: u64,
        letter: String,
        bag_total: Option<u32>,
        closes_at_us: i64,
    },
    AuctionResult {
        auction_id: u64,
        match_id: u64,
        letter: String,
        winner: Option<u64>,
        paid: i64,
    },
    Word {
        id: u64,
        match_id: u64,
        bot_id: u64,
        word: String,
    },
    Participant {
        match_id: u64,
        bot_id: u64,
        balance: i64,
        score: i64,
    },
    MatchUpdate {
        match_id: u64,
        ended: bool,
    },
    Rack {
        match_id: u64,
    },
    Poke,
    WordDone(WordDone),
    BidDone {
        match_id: u64,
        auction_id: u64,
        amount: i64,
        sent: Instant,
        result: std::result::Result<(), String>,
    },
    JoinDone(std::result::Result<(), String>),
    RegisterDone {
        tournament_id: u64,
        result: std::result::Result<(), String>,
    },
}

struct WordDone {
    match_id: u64,
    auction_id: u64,
    word: String,
    phase: &'static str,
    sent: Instant,
    result: std::result::Result<(), String>,
}

struct Msg {
    gen: u64,
    ev: Ev,
}

/// Rows applied while a subscription lands are the initial snapshot, not
/// news; the worker rebuilds from the cache instead.
fn is_news(ctx: &EventContext) -> bool {
    !matches!(
        ctx.event,
        Event::SubscribeApplied | Event::UnsubscribeApplied
    )
}

fn register_callbacks(conn: &DbConnection, tx: &Sender<Msg>, gen: u64) {
    let send = move |tx: &Sender<Msg>, ev: Ev| {
        let _ = tx.send(Msg { gen, ev });
    };

    let t = tx.clone();
    conn.db.auction().on_insert(move |ctx, a| {
        if !is_news(ctx) || !matches!(a.status, AuctionStatus::Open) {
            return;
        }
        // The cache is post-transaction here, so this is exactly the bag
        // count the server left after drawing this auction's letter.
        let bag_total = ctx
            .db
            .match_state()
            .id()
            .find(&a.match_id)
            .map(|m| m.bag_total);
        send(
            &t,
            Ev::AuctionOpen {
                auction_id: a.id,
                match_id: a.match_id,
                letter: a.letter.clone(),
                bag_total,
                closes_at_us: a.closes_at.to_micros_since_unix_epoch(),
            },
        );
    });

    let t = tx.clone();
    conn.db.auction_result().on_insert(move |ctx, r| {
        if is_news(ctx) {
            send(
                &t,
                Ev::AuctionResult {
                    auction_id: r.auction_id,
                    match_id: r.match_id,
                    letter: r.letter.clone(),
                    winner: r.winner_bot_id,
                    paid: r.paid,
                },
            );
        }
    });

    let t = tx.clone();
    conn.db.word_play().on_insert(move |ctx, w| {
        if is_news(ctx) {
            send(
                &t,
                Ev::Word {
                    id: w.id,
                    match_id: w.match_id,
                    bot_id: w.bot_id,
                    word: w.word.clone(),
                },
            );
        }
    });

    let participant = move |t: &Sender<Msg>, p: &MatchParticipant| {
        let _ = t.send(Msg {
            gen,
            ev: Ev::Participant {
                match_id: p.match_id,
                bot_id: p.bot_id,
                balance: p.balance,
                score: p.score,
            },
        });
    };
    let t = tx.clone();
    conn.db.match_participant().on_insert(move |ctx, p| {
        if is_news(ctx) {
            participant(&t, p);
        }
    });
    let t = tx.clone();
    conn.db.match_participant().on_update(move |ctx, _, p| {
        if is_news(ctx) {
            participant(&t, p);
        }
    });

    let t = tx.clone();
    conn.db.match_state().on_insert(move |ctx, m| {
        if is_news(ctx) {
            send(
                &t,
                Ev::MatchUpdate {
                    match_id: m.id,
                    ended: matches!(m.status, MatchStatus::Ended),
                },
            );
        }
    });
    let t = tx.clone();
    conn.db.match_state().on_update(move |ctx, old, m| {
        if is_news(ctx) && old.status != m.status {
            send(
                &t,
                Ev::MatchUpdate {
                    match_id: m.id,
                    ended: matches!(m.status, MatchStatus::Ended),
                },
            );
        }
    });

    let t = tx.clone();
    conn.db.my_rack().on_insert(move |ctx, h| {
        if is_news(ctx) {
            send(
                &t,
                Ev::Rack {
                    match_id: h.match_id,
                },
            );
        }
    });
    let t = tx.clone();
    conn.db.my_rack().on_update(move |ctx, _, h| {
        if is_news(ctx) {
            send(
                &t,
                Ev::Rack {
                    match_id: h.match_id,
                },
            );
        }
    });
    let t = tx.clone();
    conn.db.my_rack().on_delete(move |ctx, h| {
        if is_news(ctx) {
            send(
                &t,
                Ev::Rack {
                    match_id: h.match_id,
                },
            );
        }
    });

    macro_rules! poke_on {
        ($table:ident) => {{
            let t = tx.clone();
            conn.db.$table().on_insert(move |ctx, _| {
                if is_news(ctx) {
                    let _ = t.send(Msg { gen, ev: Ev::Poke });
                }
            });
            let t = tx.clone();
            conn.db.$table().on_delete(move |ctx, _| {
                if is_news(ctx) {
                    let _ = t.send(Msg { gen, ev: Ev::Poke });
                }
            });
        }};
    }
    poke_on!(lobby);
    poke_on!(lobby_member);
    poke_on!(tournament);
    poke_on!(tournament_entry);
    poke_on!(bot_credential);
    let t = tx.clone();
    conn.db.tournament().on_update(move |ctx, _, _| {
        if is_news(ctx) {
            let _ = t.send(Msg { gen, ev: Ev::Poke });
        }
    });
    let t = tx.clone();
    conn.db.lobby().on_update(move |ctx, _, _| {
        if is_news(ctx) {
            let _ = t.send(Msg { gen, ev: Ev::Poke });
        }
    });
}

// --------------------------------------------------------------- per match

struct OpenInfo {
    auction_id: u64,
    letter: vickrey_core::Letter,
    closes_at_ms: i64,
    retried: bool,
}

struct PendingWord {
    word: String,
    rack: Rack,
    reward: i64,
}

#[derive(Default)]
struct AuctionRec {
    seq: u32,
    letter: String,
    bag_total: u32,
    rack: String,
    balance: i64,
    score: i64,
    bid: Option<BidRec>,
    submitted: bool,
    bid_error: Option<String>,
    decision_ms: f64,
    plays_ms: f64,
    bid_ms: f64,
    ms_left: Option<i64>,
    words: Vec<String>,
    fast: bool,
    rebid: bool,
    /// The instant bid sent as the auction opened, before any valuation.
    anchor: Option<i64>,
}

struct MatchCtx {
    match_id: u64,
    me: u64,
    /// A/B arm this match plays with (`A` = champion).
    arm: String,
    kind_name: String,
    tracker: Tracker,
    brain: Brain,
    tournament_id: Option<u64>,
    last_open: u64,
    last_result: u64,
    last_word: u64,
    open: Option<OpenInfo>,
    pending: Vec<PendingWord>,
    recs: HashMap<u64, AuctionRec>,
    resumed: bool,
    letters_won: u32,
    spent: i64,
    words_played: u32,
    /// Spectator only: stand-ins for the shadow's hidden starting tiles.
    phantom: Vec<vickrey_core::Letter>,
}

/// What the worker needs from its surroundings while handling one event.
struct Env<'a> {
    conn: &'a DbConnection,
    tel: &'a Telemetry,
    tx: &'a Sender<Msg>,
    gen: u64,
    mode: &'a Mode,
    slow_ms: f64,
    fast_below_ms: i64,
    /// Dump the final partition once `bag_total` is at or below this
    /// (0 = only on the last auction). Grows with measured reducer latency.
    dump_bag: u32,
}

fn kind_of(t: &AuctionType) -> (AuctionKind, &'static str) {
    match t {
        AuctionType::Vickrey => (AuctionKind::Vickrey, "Vickrey"),
        AuctionType::FirstPrice => (AuctionKind::FirstPrice, "FirstPrice"),
    }
}

fn participants(db: &RemoteTables, match_id: u64) -> Vec<MatchParticipant> {
    let mut v: Vec<MatchParticipant> = db
        .match_participant()
        .iter()
        .filter(|p| p.match_id == match_id)
        .collect();
    v.sort_by_key(|p| p.id);
    v
}

fn cache_rack(db: &RemoteTables, match_id: u64, me: u64) -> Rack {
    let rows: Vec<Holding> = db
        .my_rack()
        .iter()
        .filter(|h| h.match_id == match_id && h.bot_id == me)
        .collect();
    rack_from_rows(rows.iter().map(|h| (h.letter.as_str(), h.count)))
}

fn bot_name(db: &RemoteTables, id: u64) -> String {
    db.bot()
        .id()
        .find(&id)
        .map(|b| b.name)
        .unwrap_or_else(|| format!("#{id}"))
}

impl MatchCtx {
    /// Pull balances, scores and our rack from the cache. In-flight words
    /// count as already played: their letters are gone and their coins in.
    fn refresh(&mut self, env: &Env) {
        let db = &env.conn.db;
        let pend_rack = self.pending.iter().fold(Rack::EMPTY, |mut acc, p| {
            acc.plus(&p.rack);
            acc
        });
        let pend_reward: i64 = self.pending.iter().map(|p| p.reward).sum();
        for p in participants(db, self.match_id) {
            let (bal, score) = if p.bot_id == self.me {
                (p.balance + pend_reward, p.score + pend_reward)
            } else {
                (p.balance, p.score)
            };
            self.tracker.on_participant(p.bot_id, bal, score);
        }
        let rack = if env.mode.is_live() {
            rack_minus(cache_rack(db, self.match_id, self.me), &pend_rack)
        } else {
            // The shadow's undisclosed deal is private; stand in phantom tiles
            // so rack sizes (and so decision cost) match a real player's.
            let me = self.tracker.me();
            let mut known = me.map(|p| p.known_rack()).unwrap_or(Rack::EMPTY);
            let hidden = me.map(|p| p.hidden_left()).unwrap_or(0);
            for &l in self.phantom.iter().take(hidden) {
                known.add(l);
            }
            rack_minus(known, &pend_rack)
        };
        self.tracker.set_my_rack(rack);
    }

    fn my_score(&self) -> i64 {
        self.tracker.me().map(|p| p.score).unwrap_or(0)
    }

    /// Feed a closed auction, once. Writes the auction's telemetry line if
    /// we made a decision on it.
    fn apply_result(
        &mut self,
        env: &Env,
        auction_id: u64,
        letter: &str,
        winner: Option<u64>,
        paid: i64,
    ) {
        if auction_id <= self.last_result {
            return;
        }
        self.last_result = auction_id;
        let Some(l) = letter_of(letter) else { return };
        self.tracker.on_auction_result(l, winner, paid);
        self.brain.observe_result(&self.tracker, l, winner, paid);
        let we_won = winner == Some(self.me);
        if we_won {
            self.letters_won += 1;
            self.spent += paid;
        }
        if self
            .open
            .as_ref()
            .is_some_and(|o| o.auction_id == auction_id)
        {
            self.open = None;
        }
        let Some(rec) = self.recs.remove(&auction_id) else {
            return;
        };
        // Under Vickrey a lost auction clears at or above our bid, so losing
        // with a bid above the price means the bid never reached the server
        // in time. The live audit found 284 of these during one server slowdown.
        if !we_won && winner.is_some() {
            if let Some(b) = rec.bid.as_ref() {
                if b.amount > paid && paid > 0 {
                    warn!(
                        "m{} GHOST LOSS on auction {auction_id}: bid {} but it cleared at {} (bid late or lost?)",
                        self.match_id, b.amount, paid
                    );
                }
            }
        }
        let db = &env.conn.db;
        let top_bid = db
            .visible_auction_top_bids()
            .auction_id()
            .find(&auction_id)
            .map(|t| t.top_bid);
        let balance_after = participants(db, self.match_id)
            .iter()
            .find(|p| p.bot_id == self.me)
            .map(|p| p.balance)
            .unwrap_or(rec.balance);
        let last = rec.bag_total == 0;
        let our_bid = rec.bid.as_ref().map(|b| b.amount).unwrap_or(0);
        env.tel.match_line(
            self.match_id,
            &MatchLine::Auction {
                v: SCHEMA_VERSION,
                ts_ms: now_ms(),
                mode: env.mode.name().into(),
                match_id: self.match_id,
                auction_id,
                seq: rec.seq,
                letter: rec.letter,
                bag_total: rec.bag_total,
                last,
                rack: rec.rack,
                balance: rec.balance,
                score: rec.score,
                bid: rec.bid,
                submitted: rec.submitted,
                bid_error: rec.bid_error,
                decision_ms: rec.decision_ms,
                plays_ms: rec.plays_ms,
                bid_ms: rec.bid_ms,
                ms_left: rec.ms_left,
                fast: rec.fast,
                rebid: rec.rebid,
                anchor: rec.anchor,
                words: rec.words,
                winner,
                we_won,
                paid,
                top_bid,
                balance_after,
            },
        );
        if we_won && env.mode.is_live() {
            info!(
                "m{} WON '{}' for {} (bid {})",
                self.match_id, letter, paid, our_bid
            );
        } else if we_won {
            info!(
                "m{} shadow won '{}' for {} (our bid would be {})",
                self.match_id, letter, paid, our_bid
            );
        }
    }

    fn apply_word(&mut self, id: u64, bot_id: u64, word: &str) {
        if id <= self.last_word {
            return;
        }
        self.last_word = id;
        self.tracker.on_word(bot_id, word);
        if bot_id == self.me {
            self.words_played += 1;
        }
    }

    /// The previous auction's result is in the same transaction as this
    /// open but its callback fires later; take it from the cache now.
    fn barrier(&mut self, env: &Env) {
        if self.last_open > self.last_result {
            if let Some(r) = env
                .conn
                .db
                .auction_result()
                .auction_id()
                .find(&self.last_open)
            {
                self.apply_result(env, r.auction_id, &r.letter, r.winner_bot_id, r.paid);
            }
        }
    }

    fn submit_word(&mut self, env: &Env, auction_id: u64, word: &str, phase: &'static str) {
        let reward = word_reward(word) as i64;
        self.pending.push(PendingWord {
            word: word.to_string(),
            rack: Rack::from_word(word),
            reward,
        });
        if !env.mode.is_live() {
            return;
        }
        let tx = env.tx.clone();
        let gen = env.gen;
        let (match_id, w) = (self.match_id, word.to_string());
        let sent = Instant::now();
        let res =
            env.conn
                .reducers
                .submit_word_then(self.match_id, word.to_string(), move |_, o| {
                    let _ = tx.send(Msg {
                        gen,
                        ev: Ev::WordDone(WordDone {
                            match_id,
                            auction_id,
                            word: w,
                            phase,
                            sent,
                            result: flatten_outcome(o),
                        }),
                    });
                });
        if let Err(e) = res {
            warn!("m{} could not send word {word}: {e}", self.match_id);
            self.pending.retain(|p| p.word != word);
        }
    }

    /// Our rack changed under an open auction in a way our in-flight words
    /// don't explain (e.g. the `my_rack` view landed after the open): price
    /// the letter again and replace the bid if it moved.
    fn maybe_rebid(&mut self, env: &Env) {
        if !env.mode.is_live() {
            return;
        }
        // With words in flight the rack event may just be our own play landing
        // ahead of its reducer callback; nothing to re-price.
        if !self.pending.is_empty() {
            return;
        }
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let (auction_id, l, closes_at_ms) = (open.auction_id, open.letter, open.closes_at_ms);
        let Some(rec) = self.recs.get(&auction_id) else {
            return;
        };
        if rec.bag_total == 0
            || rec.rack == self.tracker.my_rack.to_string()
            || closes_at_ms - now_ms() < 300
        {
            return;
        }
        let old = rec.bid.as_ref().map(|b| b.amount).unwrap_or(0);
        let t = Instant::now();
        let d = self.brain.bid(&self.tracker, l);
        let amount = d.amount.min(self.tracker.my_balance()).max(0);
        let rack = self.tracker.my_rack.to_string();
        info!(
            "m{} rack changed to {rack} mid-auction: rebid {old} -> {amount} ({:.1} ms)",
            self.match_id,
            ms(t)
        );
        if let Some(rec) = self.recs.get_mut(&auction_id) {
            rec.rack = rack;
            rec.bid = Some(BidRec {
                amount,
                value: d.value,
                lambda: d.lambda,
                p_win: d.p_win,
            });
            rec.rebid = true;
        }
        if amount != old {
            let tx = env.tx.clone();
            let sent = Instant::now();
            let (gen, match_id) = (env.gen, self.match_id);
            // A zero bid can't withdraw a sealed bid, so keep a token 1 instead.
            let send = amount.max(1);
            let _ = env
                .conn
                .reducers
                .submit_bid_then(auction_id, send, move |_, o| {
                    let _ = tx.send(Msg {
                        gen,
                        ev: Ev::BidDone {
                            sent,
                            match_id,
                            auction_id,
                            amount: send,
                            result: flatten_outcome(o),
                        },
                    });
                });
            if let Some(rec) = self.recs.get_mut(&auction_id) {
                rec.submitted = true;
            }
        }
    }

    /// Decide and act on a newly opened auction.
    fn on_open(&mut self, env: &Env, a: &Auction, bag_total: u32, lat: &mut Vec<f64>) {
        if a.id <= self.last_open {
            return;
        }
        let Some(l) = letter_of(&a.letter) else {
            return;
        };
        self.barrier(env);
        let started = Instant::now();
        self.last_open = a.id;
        self.tracker.on_auction_open(l, bag_total);
        self.refresh(env);
        let seq = self.tracker.auctions_seen;
        // Already closed (we caught up late): track it, but don't act.
        let stale = env
            .conn
            .db
            .auction()
            .id()
            .find(&a.id)
            .is_some_and(|x| !matches!(x.status, AuctionStatus::Open));
        let mut rec = AuctionRec {
            seq,
            letter: a.letter.clone(),
            bag_total,
            rack: self.tracker.my_rack.to_string(),
            balance: self.tracker.my_balance(),
            score: self.my_score(),
            ..Default::default()
        };
        let mut amount = 0;
        // Already closed when we got here (reconnect backlog, catch-up after
        // a stall): a valuation now cannot be submitted and only delays the
        // next live auction. Track it and move on.
        if stale {
            rec.decision_ms = ms(started);
            rec.ms_left = Some(a.closes_at.to_micros_since_unix_epoch() / 1000 - now_ms());
            info!(
                "m{} #{} '{}' bag {} already closed when seen ({} ms late): skipped",
                self.match_id,
                seq,
                a.letter,
                bag_total,
                -rec.ms_left.unwrap_or(0)
            );
            self.open = Some(OpenInfo {
                auction_id: a.id,
                letter: l,
                closes_at_ms: a.closes_at.to_micros_since_unix_epoch() / 1000,
                retried: false,
            });
            self.recs.insert(a.id, rec);
            return;
        }
        // Anchor bid first. Live, during server slowdowns every bid we sent
        // ~150 ms after the open came back "Auction closed" while rivals who
        // bid within milliseconds kept winning (m12305-12338: 8-24 ghost
        // losses each). Send a cheap table-based bid now; the full decision
        // below replaces it whenever it still lands in time.
        let mut anchor = 0i64;
        if bag_total != 0 && env.mode.is_live() {
            anchor = self
                .brain
                .anchor_bid(&self.tracker, l)
                .min(self.tracker.my_balance())
                .max(0);
            if anchor > 0 {
                let tx = env.tx.clone();
                let sent = Instant::now();
                let (gen, match_id, auction_id) = (env.gen, self.match_id, a.id);
                match env
                    .conn
                    .reducers
                    .submit_bid_then(a.id, anchor, move |_, o| {
                        let _ = tx.send(Msg {
                            gen,
                            ev: Ev::BidDone {
                                sent,
                                match_id,
                                auction_id,
                                amount: anchor,
                                result: flatten_outcome(o),
                            },
                        });
                    }) {
                    Ok(()) => {
                        rec.submitted = true;
                        rec.anchor = Some(anchor);
                    }
                    Err(e) => rec.bid_error = Some(format!("anchor send failed: {e}")),
                }
            }
        }
        // Inside the dump zone the end of the match is closer than a slow
        // server can be trusted to answer: cash in everything now, and keep
        // cashing whatever the next letters complete.
        let dump_zone = bag_total == 0 || (bag_total != u32::MAX && bag_total <= env.dump_bag);
        if bag_total == 0 {
            // Last auction: its letter can never be played. Cash in everything.
            let words = self.brain.final_plays(&self.tracker.my_rack);
            if !stale {
                for w in &words {
                    self.submit_word(env, a.id, w, "final");
                }
            }
            rec.words = words.iter().map(|w| w.to_string()).collect();
            rec.plays_ms = ms(started);
            info!(
                "m{} LAST auction '{}': final dump [{}] from {}",
                self.match_id,
                a.letter,
                rec.words.join(" "),
                rec.rack
            );
        } else {
            let words = if dump_zone {
                let w = self.brain.final_plays(&self.tracker.my_rack);
                if !w.is_empty() {
                    info!(
                        "m{} early dump at bag {} (zone {}): [{}] from {}",
                        self.match_id,
                        bag_total,
                        env.dump_bag,
                        w.join(" "),
                        self.tracker.my_rack
                    );
                }
                w
            } else {
                self.brain.plays(&self.tracker)
            };
            rec.plays_ms = ms(started);
            if !words.is_empty() && !stale {
                for w in &words {
                    self.submit_word(env, a.id, w, if dump_zone { "early" } else { "mid" });
                }
                self.refresh(env);
            }
            rec.words = words.iter().map(|w| w.to_string()).collect();
            // What we actually price with: after this auction's plays.
            rec.rack = self.tracker.my_rack.to_string();
            rec.balance = self.tracker.my_balance();
            rec.score = self.my_score();
            // Latency governor: if we start late (event backlog, catch-up,
            // a busy CPU), value with fewer futures rather than miss the close.
            let left = a.closes_at.to_micros_since_unix_epoch() / 1000 - now_ms();
            // `bid` re-selects its profile (multi/duel) on entry, so every
            // profile is scaled, then restored.
            let saved = (left < env.fast_below_ms).then(|| {
                let saved = (
                    self.brain.params.clone(),
                    self.brain.multi.clone(),
                    self.brain.duel.clone(),
                );
                let shrink = |p: &mut vickrey_brain::Params| {
                    p.samples = (p.samples / 4).max(4);
                    p.table_samples = (p.table_samples / 4).max(2);
                };
                shrink(&mut self.brain.params);
                shrink(&mut self.brain.multi);
                if let Some(d) = self.brain.duel.as_mut() {
                    shrink(d);
                }
                saved
            });
            let t_bid = Instant::now();
            let d = self.brain.bid(&self.tracker, l);
            rec.bid_ms = ms(t_bid);
            if let Some((params, multi, duel)) = saved {
                self.brain.params = params;
                self.brain.multi = multi;
                self.brain.duel = duel;
                rec.fast = true;
                info!(
                    "m{} late start ({left} ms left): valued with reduced samples",
                    self.match_id
                );
            }
            amount = d.amount.min(self.tracker.my_balance()).max(0);
            rec.bid = Some(BidRec {
                amount,
                value: d.value,
                lambda: d.lambda,
                p_win: d.p_win,
            });
            // A sealed bid cannot be withdrawn: if the anchor went out and
            // the full decision is 0, lower it to a token 1 instead.
            let send = if anchor > 0 { amount.max(1) } else { amount };
            if send > 0 && send != anchor && env.mode.is_live() && !stale {
                let tx = env.tx.clone();
                let sent = Instant::now();
                let (gen, match_id, auction_id) = (env.gen, self.match_id, a.id);
                match env.conn.reducers.submit_bid_then(a.id, send, move |_, o| {
                    let _ = tx.send(Msg {
                        gen,
                        ev: Ev::BidDone {
                            sent,
                            match_id,
                            auction_id,
                            amount: send,
                            result: flatten_outcome(o),
                        },
                    });
                }) {
                    Ok(()) => rec.submitted = true,
                    Err(e) => rec.bid_error = Some(format!("send failed: {e}")),
                }
            }
        }
        rec.decision_ms = ms(started);
        rec.ms_left = Some(a.closes_at.to_micros_since_unix_epoch() / 1000 - now_ms());
        lat.push(rec.decision_ms);
        if rec.decision_ms > env.slow_ms {
            warn!(
                "m{} decision took {:.0} ms (> {:.0} ms budget)",
                self.match_id, rec.decision_ms, env.slow_ms
            );
        }
        if bag_total > 0 {
            let b = rec.bid.clone().unwrap_or_default();
            info!(
                "m{} #{} '{}' bag {} rack {} bal {} -> bid {} (v {:.1} lam {:.2} p {:.2}) {:.1} ms{}{}",
                self.match_id,
                seq,
                a.letter,
                bag_total,
                rec.rack,
                rec.balance,
                amount,
                b.value,
                b.lambda,
                b.p_win,
                rec.decision_ms,
                if rec.words.is_empty() { String::new() } else { format!(" play [{}]", rec.words.join(" ")) },
                if stale { " (stale)" } else { "" },
            );
        }
        self.open = Some(OpenInfo {
            auction_id: a.id,
            letter: l,
            closes_at_ms: a.closes_at.to_micros_since_unix_epoch() / 1000,
            retried: false,
        });
        self.recs.insert(a.id, rec);
    }
}

/// A bid rejected as late counts only if we sent it with this much time left.
const LATE_REJECT_MIN_LEFT_MS: i64 = 400;
/// Racks in matches with at most this many tiles left in the bag are dumped
/// before a forced reconnect (a reconnect under lag takes 4-12 s).
const LAG_DUMP_BAG: u32 = 12;

/// Connection-health thresholds, overridable without a rebuild through the
/// environment (`VICKREY_LAG_RTT_MS`, `VICKREY_LAG_STRIKES`, e.g. from the
/// launchd plist). Bids are submitted by every bot in the same instant an
/// auction opens, so their round trips run well above word round trips even
/// on a healthy connection: live, bids took 500-600 ms while words took 160
/// ms and nothing was rejected. Rejections began near 850 ms.
fn lag_thresholds() -> (f64, u32) {
    static T: std::sync::OnceLock<(f64, u32)> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        let rtt = std::env::var("VICKREY_LAG_RTT_MS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(800.0);
        let strikes = std::env::var("VICKREY_LAG_STRIKES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(3);
        (rtt, strikes)
    })
}

/// How early to dump the final partition. The server runs reducers one at
/// a time per database; at peak load a call can take seconds, and a dump
/// that lands after the last auction closes scores nothing.
/// Auctions-before-the-end at which to dump, from recent round trips:
/// dump when `bag × 1 s + ~0.85 s` (time left) is under `2 × p90 + 0.3 s`.
pub(crate) fn dump_bag_for(rtts: &std::collections::VecDeque<f64>) -> u32 {
    if rtts.is_empty() {
        return 0;
    }
    let mut v: Vec<f64> = rtts.iter().copied().collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p90 = v[((v.len() - 1) as f64 * 0.9).round() as usize];
    let need_ms = 2.0 * p90 + 300.0 - 850.0;
    if need_ms <= 0.0 {
        0
    } else {
        ((need_ms / 1000.0).ceil() as u32).min(12)
    }
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

// ------------------------------------------------------------------ engine

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum ConnState {
    Down,
    Connecting,
    Ready,
}

pub struct Engine {
    opts: RunOpts,
    tel: Telemetry,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    conn: Option<DbConnection>,
    gen: u64,
    state: ConnState,
    identity: Option<Identity>,
    my_bot: Option<u64>,
    matches: HashMap<u64, MatchCtx>,
    finished: HashSet<u64>,
    ignored: HashSet<u64>,
    lat: Vec<f64>,
    /// Recent reducer round trips (ms), words and bids alike.
    rtts: std::collections::VecDeque<f64>,
    floor: u64,
    phase2: Option<SubscriptionHandle>,
    phase2_next: Option<(u64, SubscriptionHandle)>,
    floor_checked: Instant,
    failures: u32,
    reconnects: u64,
    reconnect_at: Instant,
    connecting_since: Instant,
    /// Consecutive bids that came back slow or rejected as late.
    lag_strikes: u32,
    last_forced_reconnect: Instant,
    /// A forced reconnect deferred so that racks dumped on the old
    /// connection have time to leave before it is closed.
    lag_reconnect_at: Option<Instant>,
    last_join: Option<Instant>,
    register_tries: HashMap<u64, Instant>,
    poke: bool,
    last_poke: Instant,
    last_watchdog: Instant,
    last_heartbeat: Instant,
    last_event: Instant,
    started: Instant,
    matches_finished: u64,
}

impl Engine {
    pub fn new(opts: RunOpts) -> Self {
        let root = match &opts.mode {
            Mode::Live => opts.common.runs_dir.clone(),
            Mode::Spectate(_) => opts.common.runs_dir.join("spectate"),
        };
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        Engine {
            tel: Telemetry::new(root),
            opts,
            tx,
            rx,
            conn: None,
            gen: 0,
            state: ConnState::Down,
            identity: None,
            my_bot: None,
            matches: HashMap::new(),
            finished: HashSet::new(),
            ignored: HashSet::new(),
            lat: Vec::new(),
            rtts: std::collections::VecDeque::new(),
            floor: 0,
            phase2: None,
            phase2_next: None,
            floor_checked: now,
            failures: 0,
            reconnects: 0,
            reconnect_at: now,
            connecting_since: now,
            lag_strikes: 0,
            last_forced_reconnect: now - Duration::from_secs(600),
            lag_reconnect_at: None,
            last_join: None,
            register_tries: HashMap::new(),
            poke: false,
            last_poke: now,
            last_watchdog: now,
            last_heartbeat: now - Duration::from_secs(60),
            last_event: now,
            started: now,
            matches_finished: 0,
        }
    }

    pub fn run(mut self) -> Result<RunStats> {
        let t = Instant::now();
        let n = vickrey_core::dictionary().len();
        info!("dictionary ready: {n} words in {:.0} ms", ms(t));
        if self.opts.mode.is_live() && self.opts.common.bot_token()?.is_none() {
            bail!(
                "no bot token (looked at {} and $BOT_TOKEN). Run `vickrey setup --team <TEAM> --bot <BOT>` first, \
                 or use `vickrey run --spectate all` to watch without one.",
                self.opts.common.token_path().display()
            );
        }
        prewarm(&self.opts.common);
        info!(
            "mode {} | runs dir {} | lobby {} | tournaments {}",
            self.opts.mode.name(),
            self.tel.root().display(),
            if self.opts.no_lobby || !self.opts.mode.is_live() {
                "off"
            } else {
                "auto"
            },
            if self.opts.no_tournaments || !self.opts.mode.is_live() {
                "off"
            } else {
                "auto"
            },
        );
        self.connect();
        loop {
            if let Some(d) = self.opts.duration {
                if self.started.elapsed() >= d {
                    info!("duration reached; stopping");
                    break;
                }
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(msg) => {
                    if msg.gen == self.gen {
                        self.last_event = Instant::now();
                        self.handle(msg.ev)?;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("event channel closed"),
            }
            self.tick()?;
        }
        if let Some(c) = self.conn.take() {
            let _ = c.disconnect();
        }
        self.write_heartbeat("stopped");
        Ok(RunStats {
            latencies_ms: self.lat,
            matches_finished: self.matches_finished,
        })
    }

    // ------------------------------------------------------- connection

    fn connect(&mut self) {
        self.gen += 1;
        let gen = self.gen;
        self.state = ConnState::Connecting;
        self.connecting_since = Instant::now();
        self.phase2 = None;
        self.phase2_next = None;
        let c = &self.opts.common;
        let token = if self.opts.mode.is_live() {
            match c.bot_token() {
                Ok(Some(t)) => Some(t),
                Ok(None) => None,
                Err(e) => {
                    warn!("token: {e:#}");
                    None
                }
            }
        } else {
            None // spectators connect anonymously so the top-bid view is visible
        };
        info!(
            "connecting to {} / {} ({})",
            c.host,
            c.db,
            if token.is_some() {
                "bot token"
            } else {
                "anonymous"
            }
        );
        let (t1, t2, t3) = (self.tx.clone(), self.tx.clone(), self.tx.clone());
        let built = DbConnection::builder()
            .with_uri(c.host.as_str())
            .with_database_name(c.db.clone())
            .with_token(token)
            .on_connect(move |_, identity, _| {
                let _ = t1.send(Msg {
                    gen,
                    ev: Ev::Connected(identity),
                });
            })
            .on_connect_error(move |_, e| {
                let _ = t2.send(Msg {
                    gen,
                    ev: Ev::ConnectFailed(e.to_string()),
                });
            })
            .on_disconnect(move |_, e| {
                let _ = t3.send(Msg {
                    gen,
                    ev: Ev::Disconnected(e.map(|e| e.to_string())),
                });
            })
            .build();
        let conn = match built {
            Ok(c) => c,
            Err(e) => {
                self.schedule_reconnect(format!("connect failed: {e}"));
                return;
            }
        };
        register_callbacks(&conn, &self.tx, gen);
        conn.run_threaded();
        let mut q: Vec<String> = [
            "bot",
            "bot_credential",
            "bot_stats",
            "lobby",
            "lobby_member",
            "tournament",
            "tournament_entry",
            "tournament_match",
            "match_state",
            "match_participant",
            "my_rack",
        ]
        .iter()
        .map(|t| format!("SELECT * FROM {t}"))
        .collect();
        if !self.opts.mode.is_live() {
            q.retain(|s| !s.ends_with("my_rack"));
        }
        let (ta, tb) = (self.tx.clone(), self.tx.clone());
        conn.subscription_builder()
            .on_applied(move |_| {
                let _ = ta.send(Msg {
                    gen,
                    ev: Ev::Phase1,
                });
            })
            .on_error(move |_, e| {
                let _ = tb.send(Msg {
                    gen,
                    ev: Ev::SubError(e.to_string()),
                });
            })
            .subscribe(q);
        self.conn = Some(conn);
    }

    fn schedule_reconnect(&mut self, why: String) {
        if self.state == ConnState::Down {
            return; // already handled for this connection
        }
        if let Some(c) = self.conn.take() {
            let _ = c.disconnect();
        }
        self.state = ConnState::Down;
        self.failures += 1;
        let backoff = Duration::from_secs((1u64 << self.failures.min(5)).min(30));
        warn!(
            "{why}; reconnecting in {}s (attempt {})",
            backoff.as_secs(),
            self.failures
        );
        self.reconnect_at = Instant::now() + backoff;
    }

    fn compute_floor(db: &RemoteTables) -> u64 {
        let mut min_running = None::<u64>;
        let mut max_id = 0;
        for m in db.match_state().iter() {
            max_id = max_id.max(m.id);
            if matches!(m.status, MatchStatus::Running) {
                min_running = Some(min_running.map_or(m.id, |x: u64| x.min(m.id)));
            }
        }
        min_running.unwrap_or(max_id + 1)
    }

    fn phase2_queries(&self, floor: u64) -> Vec<String> {
        let mut q: Vec<String> = ["auction", "auction_result", "word_play"]
            .iter()
            .map(|t| format!("SELECT * FROM {t} WHERE match_id >= {floor}"))
            .collect();
        if !self.opts.mode.is_live() {
            q.push("SELECT * FROM visible_auction_top_bids".into());
        }
        q
    }

    fn subscribe_phase2(&self, floor: u64, done: Ev) -> Option<SubscriptionHandle> {
        let conn = self.conn.as_ref()?;
        let gen = self.gen;
        let (ta, tb) = (self.tx.clone(), self.tx.clone());
        let mut done = Some(done);
        Some(
            conn.subscription_builder()
                .on_applied(move |_| {
                    if let Some(ev) = done.take() {
                        let _ = ta.send(Msg { gen, ev });
                    }
                })
                .on_error(move |_, e| {
                    let _ = tb.send(Msg {
                        gen,
                        ev: Ev::SubError(e.to_string()),
                    });
                })
                .subscribe(self.phase2_queries(floor)),
        )
    }

    // ----------------------------------------------------------- events

    fn handle(&mut self, ev: Ev) -> Result<()> {
        match ev {
            Ev::Connected(id) => {
                info!("connected as {}", id.to_hex());
                self.identity = Some(id);
            }
            Ev::ConnectFailed(e) => self.schedule_reconnect(format!("connection error: {e}")),
            Ev::Disconnected(e) => {
                if self.state != ConnState::Down {
                    self.reconnects += 1;
                }
                self.schedule_reconnect(format!(
                    "disconnected ({})",
                    e.unwrap_or_else(|| "clean".into())
                ));
            }
            Ev::SubError(e) => self.schedule_reconnect(format!("subscription error: {e}")),
            Ev::Phase1 => self.on_phase1()?,
            Ev::Ready => self.on_ready(),
            Ev::FloorApplied => {
                if let Some((floor, h)) = self.phase2_next.take() {
                    if let Some(old) = self.phase2.replace(h) {
                        let _ = old.unsubscribe();
                    }
                    debug!("history floor raised to match {floor}");
                    self.floor = floor;
                }
            }
            _ if self.state != ConnState::Ready => {}
            Ev::AuctionOpen {
                auction_id,
                match_id,
                letter,
                bag_total,
                closes_at_us,
            } => {
                if self.ensure_ctx(match_id) {
                    self.on_open_event(match_id, auction_id, letter, bag_total, closes_at_us);
                }
            }
            Ev::AuctionResult {
                auction_id,
                match_id,
                letter,
                winner,
                paid,
            } => {
                if let (Some(conn), Some(ctx)) =
                    (self.conn.as_ref(), self.matches.get_mut(&match_id))
                {
                    let env = Env {
                        conn,
                        tel: &self.tel,
                        tx: &self.tx,
                        gen: self.gen,
                        mode: &self.opts.mode,
                        slow_ms: self.opts.slow_ms,
                        fast_below_ms: self.opts.fast_below_ms,
                        dump_bag: dump_bag_for(&self.rtts),
                    };
                    ctx.apply_result(&env, auction_id, &letter, winner, paid);
                }
            }
            Ev::Word {
                id,
                match_id,
                bot_id,
                word,
            } => {
                if let Some(ctx) = self.matches.get_mut(&match_id) {
                    ctx.apply_word(id, bot_id, &word);
                }
            }
            Ev::Participant {
                match_id,
                bot_id,
                balance,
                score,
            } => {
                if Some(bot_id) == self.my_bot && !self.matches.contains_key(&match_id) {
                    self.ensure_ctx(match_id);
                }
                if let Some(ctx) = self.matches.get_mut(&match_id) {
                    if bot_id == ctx.me {
                        let pend: i64 = ctx.pending.iter().map(|p| p.reward).sum();
                        ctx.tracker
                            .on_participant(bot_id, balance + pend, score + pend);
                    } else {
                        ctx.tracker.on_participant(bot_id, balance, score);
                    }
                }
            }
            Ev::MatchUpdate { match_id, ended } => {
                if ended {
                    if self.matches.contains_key(&match_id) {
                        self.finalize(match_id);
                    }
                    self.ignored.remove(&match_id);
                } else {
                    self.ensure_ctx(match_id);
                }
            }
            Ev::Rack { match_id } => {
                if self.opts.mode.is_live() && self.ensure_ctx(match_id) {
                    if let (Some(conn), Some(ctx)) =
                        (self.conn.as_ref(), self.matches.get_mut(&match_id))
                    {
                        let env = Env {
                            conn,
                            tel: &self.tel,
                            tx: &self.tx,
                            gen: self.gen,
                            mode: &self.opts.mode,
                            slow_ms: self.opts.slow_ms,
                            fast_below_ms: self.opts.fast_below_ms,
                            dump_bag: dump_bag_for(&self.rtts),
                        };
                        ctx.refresh(&env);
                        ctx.maybe_rebid(&env);
                    }
                }
            }
            Ev::Poke => self.poke = true,
            Ev::WordDone(w) => self.on_word_done(w),
            Ev::BidDone {
                match_id,
                auction_id,
                amount,
                sent,
                result,
            } => {
                let rtt = ms(sent);
                self.note_rtt(rtt);
                let late = self.on_bid_done(match_id, auction_id, amount, result);
                self.lag_check(rtt, late);
            }
            Ev::JoinDone(r) => match r {
                Ok(()) => info!("joined the lobby"),
                Err(e) => info!("join_lobby refused: {e}"),
            },
            Ev::RegisterDone {
                tournament_id,
                result,
            } => match result {
                Ok(()) => warn!("*** REGISTERED for tournament {tournament_id} ***"),
                Err(e) => warn!("register_for_tournament {tournament_id} refused: {e}"),
            },
        }
        Ok(())
    }

    fn on_phase1(&mut self) -> Result<()> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(());
        };
        let identity = self.identity.or_else(|| conn.try_identity());
        match &self.opts.mode {
            Mode::Live => {
                let Some(id) = identity else {
                    bail!("connected without an identity")
                };
                match conn.db.bot_credential().identity().find(&id) {
                    Some(cred) => {
                        let name = bot_name(&conn.db, cred.bot_id);
                        info!("acting as bot '{name}' (id {})", cred.bot_id);
                        self.my_bot = Some(cred.bot_id);
                    }
                    None => bail!(
                        "identity {} is not a credential for any bot. Run `vickrey setup --team <TEAM> --bot <BOT>` \
                         or point --token-file / BOT_TOKEN at a bot token.",
                        id.to_hex()
                    ),
                }
            }
            Mode::Spectate(Shadow::Bot(b)) => {
                info!(
                    "spectating: shadowing bot '{}' (id {b}); no reducers will be called",
                    bot_name(&conn.db, *b)
                );
                self.my_bot = Some(*b);
            }
            Mode::Spectate(Shadow::All) => {
                info!("spectating every running match; no reducers will be called")
            }
        }
        let floor = Self::compute_floor(&conn.db);
        self.floor = floor;
        self.floor_checked = Instant::now();
        debug!("subscribing to match history from match {floor}");
        self.phase2 = self.subscribe_phase2(floor, Ev::Ready);
        Ok(())
    }

    fn on_ready(&mut self) {
        self.state = ConnState::Ready;
        self.failures = 0;
        // We may have missed events while away: rebuild every match from scratch.
        if !self.matches.is_empty() {
            info!("resyncing {} match(es) after reconnect", self.matches.len());
        }
        self.matches.clear();
        self.ignored.clear();
        let running: Vec<u64> = self
            .conn
            .as_ref()
            .map(|c| {
                c.db.match_state()
                    .iter()
                    .filter(|m| matches!(m.status, MatchStatus::Running))
                    .map(|m| m.id)
                    .collect()
            })
            .unwrap_or_default();
        info!("ready: {} running match(es) on the server", running.len());
        for m in running {
            self.ensure_ctx(m);
        }
        self.autopilot();
        self.write_heartbeat("connected");
    }

    /// Who we play as in `match_id`, if anyone.
    fn wants(&self, db: &RemoteTables, match_id: u64) -> Option<u64> {
        let parts = participants(db, match_id);
        match &self.opts.mode {
            Mode::Live | Mode::Spectate(Shadow::Bot(_)) => {
                let me = self.my_bot?;
                parts.iter().any(|p| p.bot_id == me).then_some(me)
            }
            Mode::Spectate(Shadow::All) => {
                let rating = |id: u64| {
                    db.bot_stats()
                        .bot_id()
                        .find(&id)
                        .map(|s| s.rating)
                        .unwrap_or(0)
                };
                let real = |id: u64| db.bot().id().find(&id).is_some_and(|b| !b.is_simulated);
                parts
                    .iter()
                    .map(|p| p.bot_id)
                    .max_by_key(|&id| (real(id), rating(id), std::cmp::Reverse(id)))
            }
        }
    }

    /// Make sure we track `match_id` if it is ours. Returns whether we do.
    fn ensure_ctx(&mut self, match_id: u64) -> bool {
        if self.matches.contains_key(&match_id) {
            return true;
        }
        if self.finished.contains(&match_id) || self.ignored.contains(&match_id) {
            return false;
        }
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        let Some(m) = conn.db.match_state().id().find(&match_id) else {
            return false;
        };
        if !matches!(m.status, MatchStatus::Running) {
            return false;
        }
        let Some(me) = self.wants(&conn.db, match_id) else {
            self.ignored.insert(match_id);
            return false;
        };
        let env = Env {
            conn,
            tel: &self.tel,
            tx: &self.tx,
            gen: self.gen,
            mode: &self.opts.mode,
            slow_ms: self.opts.slow_ms,
            fast_below_ms: self.opts.fast_below_ms,
            dump_bag: dump_bag_for(&self.rtts),
        };
        let (mut ctx, open) = build_ctx(&env, &self.opts.common, &m, me);
        info!(
            "m{match_id}: {} {}-player match as '{}'{}{}",
            ctx.kind_name,
            ctx.tracker.n_players(),
            bot_name(&conn.db, me),
            if ctx.resumed {
                format!(" (joined at auction {})", ctx.tracker.auctions_seen)
            } else {
                String::new()
            },
            ctx.tournament_id
                .map(|t| format!(" [tournament {t}]"))
                .unwrap_or_default(),
        );
        if let Some(a) = open {
            ctx.on_open(&env, &a, m.bag_total, &mut self.lat);
        }
        self.matches.insert(match_id, ctx);
        true
    }

    fn on_open_event(
        &mut self,
        match_id: u64,
        auction_id: u64,
        letter: String,
        bag: Option<u32>,
        closes_at_us: i64,
    ) {
        let (Some(conn), Some(ctx)) = (self.conn.as_ref(), self.matches.get_mut(&match_id)) else {
            return;
        };
        let bag_total = bag
            .or_else(|| {
                conn.db
                    .match_state()
                    .id()
                    .find(&match_id)
                    .map(|m| m.bag_total)
            })
            .unwrap_or(u32::MAX);
        let a = Auction {
            id: auction_id,
            match_id,
            letter,
            opens_at: spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(closes_at_us),
            closes_at: spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(closes_at_us),
            status: AuctionStatus::Open,
        };
        let env = Env {
            conn,
            tel: &self.tel,
            tx: &self.tx,
            gen: self.gen,
            mode: &self.opts.mode,
            slow_ms: self.opts.slow_ms,
            fast_below_ms: self.opts.fast_below_ms,
            dump_bag: dump_bag_for(&self.rtts),
        };
        ctx.on_open(&env, &a, bag_total, &mut self.lat);
    }

    /// Reducer-path health. Both live ghost storms (21:05 and 21:56 UTC on
    /// Oct 24) ran on a connection whose reducer calls took 600-1500 ms from
    /// its first second while auction opens still arrived on time, and ended
    /// the moment the connection was replaced. Three consecutive bids that
    /// are slow or rejected as late force a fresh connection, at most once a
    /// minute; matches are rebuilt from the server on reconnect.
    fn lag_check(&mut self, rtt: f64, rejected_late: bool) {
        let (lag_rtt_ms, lag_strikes) = lag_thresholds();
        if rtt > lag_rtt_ms || rejected_late {
            self.lag_strikes += 1;
        } else {
            self.lag_strikes = 0;
        }
        if self.lag_strikes >= lag_strikes
            && self.state == ConnState::Ready
            && self.lag_reconnect_at.is_none()
            && self.last_forced_reconnect.elapsed() > Duration::from_secs(60)
        {
            warn!(
                "reducer path lagging ({} consecutive bids slow or late, last {rtt:.0} ms): replacing the connection",
                self.lag_strikes
            );
            self.lag_strikes = 0;
            self.last_forced_reconnect = Instant::now();
            // A reconnect under lag takes 4-12 s. Live (m13846) one fired at
            // auction 63 and the match ended before we were back: the rack
            // was never dumped and we fell from 2nd to 5th. Cash in every
            // rack that is near its end on the old connection first, then
            // close it a moment later.
            let mut dumped = 0usize;
            if let Some(conn) = self.conn.as_ref() {
                let env = Env {
                    conn,
                    tel: &self.tel,
                    tx: &self.tx,
                    gen: self.gen,
                    mode: &self.opts.mode,
                    slow_ms: self.opts.slow_ms,
                    fast_below_ms: self.opts.fast_below_ms,
                    dump_bag: dump_bag_for(&self.rtts),
                };
                for ctx in self.matches.values_mut() {
                    if ctx.tracker.bag_left > LAG_DUMP_BAG {
                        continue;
                    }
                    let words = ctx.brain.final_plays(&ctx.tracker.my_rack);
                    if words.is_empty() {
                        continue;
                    }
                    let aid = ctx.open.as_ref().map(|o| o.auction_id).unwrap_or(0);
                    warn!(
                        "m{} dumping [{}] before the reconnect (bag {})",
                        ctx.match_id,
                        words.join(" "),
                        ctx.tracker.bag_left
                    );
                    for w in &words {
                        ctx.submit_word(&env, aid, w, "final");
                    }
                    dumped += words.len();
                }
            }
            if dumped > 0 {
                self.lag_reconnect_at = Some(Instant::now() + Duration::from_millis(1500));
            } else {
                self.force_reconnect();
            }
        }
    }

    /// Replace the connection now: not a failure of ours, so no backoff.
    fn force_reconnect(&mut self) {
        self.schedule_reconnect("reducer path lagging".into());
        self.failures = 0;
        self.reconnect_at = Instant::now();
    }

    fn note_rtt(&mut self, rtt: f64) {
        self.rtts.push_back(rtt);
        while self.rtts.len() > 40 {
            self.rtts.pop_front();
        }
    }

    fn on_word_done(&mut self, w: WordDone) {
        let rtt = ms(w.sent);
        self.note_rtt(rtt);
        if let (Some(conn), Some(ctx)) = (self.conn.as_ref(), self.matches.get_mut(&w.match_id)) {
            if let Some(i) = ctx.pending.iter().position(|p| p.word == w.word) {
                ctx.pending.remove(i);
            }
            // The cache already reflects the play (or its rejection): resync.
            let env = Env {
                conn,
                tel: &self.tel,
                tx: &self.tx,
                gen: self.gen,
                mode: &self.opts.mode,
                slow_ms: self.opts.slow_ms,
                fast_below_ms: self.opts.fast_below_ms,
                dump_bag: dump_bag_for(&self.rtts),
            };
            ctx.refresh(&env);
        }
        match &w.result {
            Ok(()) => info!(
                "m{} played {} (+{}) in {:.0} ms",
                w.match_id,
                w.word,
                word_reward(&w.word),
                rtt
            ),
            Err(e) => warn!("m{} word {} rejected: {e}", w.match_id, w.word),
        }
        self.tel.match_line(
            w.match_id,
            &MatchLine::Word {
                v: SCHEMA_VERSION,
                ts_ms: now_ms(),
                mode: self.opts.mode.name().into(),
                match_id: w.match_id,
                auction_id: w.auction_id,
                word: w.word.clone(),
                phase: w.phase.into(),
                expected_reward: word_reward(&w.word),
                ok: w.result.is_ok(),
                error: w.result.err(),
                rtt_ms: rtt,
            },
        );
    }

    /// Handle a bid's reducer result. Returns true when the bid was rejected
    /// as late although we sent it with time to spare: the reducer path, not
    /// our decision, was slow.
    fn on_bid_done(
        &mut self,
        match_id: u64,
        auction_id: u64,
        amount: i64,
        result: std::result::Result<(), String>,
    ) -> bool {
        let Err(e) = result else { return false };
        warn!("m{match_id} bid {amount} on auction {auction_id} rejected: {e}");
        let (Some(conn), Some(ctx)) = (self.conn.as_ref(), self.matches.get_mut(&match_id)) else {
            return false;
        };
        let mut late = false;
        if let Some(rec) = ctx.recs.get_mut(&auction_id) {
            rec.bid_error = Some(e.clone());
            late = (e.contains("Auction closed") || e.contains("window expired"))
                && rec.ms_left.unwrap_or(0) > LATE_REJECT_MIN_LEFT_MS;
        }
        // A word we counted on failed, so the coins never arrived: rebid what we have.
        let still_open = ctx
            .open
            .as_ref()
            .is_some_and(|o| o.auction_id == auction_id && !o.retried);
        if e.contains("Insufficient balance") && still_open {
            let bal = participants(&conn.db, match_id)
                .iter()
                .find(|p| p.bot_id == ctx.me)
                .map(|p| p.balance)
                .unwrap_or(0);
            let retry = amount.min(bal);
            if let Some(o) = ctx.open.as_mut() {
                o.retried = true;
            }
            if retry > 0 {
                let (tx, gen) = (self.tx.clone(), self.gen);
                let sent = Instant::now();
                let _ = conn
                    .reducers
                    .submit_bid_then(auction_id, retry, move |_, o| {
                        let _ = tx.send(Msg {
                            gen,
                            ev: Ev::BidDone {
                                sent,
                                match_id,
                                auction_id,
                                amount: retry,
                                result: flatten_outcome(o),
                            },
                        });
                    });
                info!("m{match_id} rebid {retry} on auction {auction_id}");
            }
        }
        late
    }

    fn finalize(&mut self, match_id: u64) {
        let Some(mut ctx) = self.matches.remove(&match_id) else {
            return;
        };
        self.finished.insert(match_id);
        self.matches_finished += 1;
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let env = Env {
            conn,
            tel: &self.tel,
            tx: &self.tx,
            gen: self.gen,
            mode: &self.opts.mode,
            slow_ms: self.opts.slow_ms,
            fast_below_ms: self.opts.fast_below_ms,
            dump_bag: dump_bag_for(&self.rtts),
        };
        ctx.barrier(&env);
        let parts = participants(&conn.db, match_id);
        let mine = parts.iter().find(|p| p.bot_id == ctx.me);
        let (our_score, our_balance) = mine
            .map(|p| (p.score, p.balance))
            .unwrap_or((ctx.my_score(), 0));
        let place = place_of(
            our_score,
            parts.iter().filter(|p| p.bot_id != ctx.me).map(|p| p.score),
        );
        let mut scores: Vec<i64> = parts.iter().map(|p| p.score).collect();
        scores.sort_unstable_by(|a, b| b.cmp(a));
        let winner_score = scores.first().copied().unwrap_or(0);
        let players: Vec<PlayerRec> = parts
            .iter()
            .map(|p| PlayerRec {
                bot_id: p.bot_id,
                name: bot_name(&conn.db, p.bot_id),
                score: p.score,
                balance: p.balance,
            })
            .collect();
        let ts = now_ms();
        let mode = self.opts.mode.name().to_string();
        self.tel.match_line(
            match_id,
            &MatchLine::End {
                v: SCHEMA_VERSION,
                ts_ms: ts,
                mode: mode.clone(),
                match_id,
                my_bot_id: ctx.me,
                auction_type: ctx.kind_name.clone(),
                n_players: parts.len(),
                place,
                our_score,
                our_balance,
                winner_score,
                letters_won: ctx.letters_won,
                spent: ctx.spent,
                words_played: ctx.words_played,
                auctions_seen: ctx.tracker.auctions_seen,
                resumed: ctx.resumed,
                tournament_id: ctx.tournament_id,
                participants: players,
            },
        );
        self.tel.summary(&SummaryLine {
            v: SCHEMA_VERSION,
            ts,
            mode,
            match_id,
            my_bot_id: ctx.me,
            n_players: parts.len(),
            auction_type: ctx.kind_name.clone(),
            our_score,
            our_balance,
            place,
            winner_score,
            scores,
            letters_won: ctx.letters_won,
            spent: ctx.spent,
            words_played: ctx.words_played,
            resumed: ctx.resumed,
            tournament_id: ctx.tournament_id,
            arm: ctx.arm.clone(),
        });
        info!(
            "m{match_id} ENDED: '{}' placed {place}/{} with {our_score} (winner {winner_score}); won {} letters for {}",
            bot_name(&conn.db, ctx.me),
            parts.len(),
            ctx.letters_won,
            ctx.spent
        );
        // Live A/B: let the league decide between champion and challenger.
        if self.opts.mode.is_live() {
            let common = &self.opts.common;
            if let Some(ab) = common.ab() {
                if let Some(msg) =
                    crate::ab::maybe_decide(&common.config_dir, &common.runs_dir, &ab)
                {
                    warn!("{msg}");
                }
            }
            if let Some(msg) = crate::ab::advance_queue(&common.config_dir) {
                warn!("{msg}");
            }
        }
        self.autopilot();
    }

    // ------------------------------------------------------- autopilots

    fn autopilot(&mut self) {
        if self.state != ConnState::Ready || !self.opts.mode.is_live() {
            return;
        }
        if !self.opts.no_tournaments {
            self.tournament_check();
        }
        if !self.opts.no_lobby {
            self.lobby_check();
        }
    }

    fn lobby_check(&mut self) {
        let (Some(conn), Some(me)) = (self.conn.as_ref(), self.my_bot) else {
            return;
        };
        let db = &conn.db;
        let in_running = db.match_participant().iter().any(|p| {
            p.bot_id == me
                && db
                    .match_state()
                    .id()
                    .find(&p.match_id)
                    .is_some_and(|m| matches!(m.status, MatchStatus::Running))
        });
        if in_running {
            return;
        }
        let held = db.tournament_entry().iter().any(|e| {
            e.bot_id == me
                && !e.eliminated
                && db
                    .tournament()
                    .id()
                    .find(&e.tournament_id)
                    .is_some_and(|t| {
                        matches!(
                            t.status,
                            TournamentStatus::Swiss
                                | TournamentStatus::Bracket
                                | TournamentStatus::Final
                        )
                    })
        });
        if held {
            return;
        }
        let open = db
            .lobby()
            .iter()
            .filter(|l| matches!(l.status, LobbyStatus::Open))
            .max_by_key(|l| l.id);
        if let Some(l) = &open {
            if db
                .lobby_member()
                .iter()
                .any(|m| m.lobby_id == l.id && m.bot_id == me)
            {
                return;
            }
        }
        if self
            .last_join
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        self.last_join = Some(Instant::now());
        info!(
            "joining lobby{}",
            open.map(|l| format!(" {}", l.id)).unwrap_or_default()
        );
        let (tx, gen) = (self.tx.clone(), self.gen);
        if let Err(e) = conn.reducers.join_lobby_then(move |_, o| {
            let _ = tx.send(Msg {
                gen,
                ev: Ev::JoinDone(flatten_outcome(o)),
            });
        }) {
            warn!("join_lobby send failed: {e}");
        }
    }

    fn tournament_check(&mut self) {
        let (Some(conn), Some(me)) = (self.conn.as_ref(), self.my_bot) else {
            return;
        };
        let db = &conn.db;
        let open: Vec<u64> = db
            .tournament()
            .iter()
            .filter(|t| matches!(t.status, TournamentStatus::Registration))
            .map(|t| t.id)
            .collect();
        for tid in open {
            if db
                .tournament_entry()
                .iter()
                .any(|e| e.tournament_id == tid && e.bot_id == me)
            {
                continue;
            }
            if self
                .register_tries
                .get(&tid)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
            {
                continue;
            }
            self.register_tries.insert(tid, Instant::now());
            warn!("*** tournament {tid} is open for registration: registering ***");
            let (tx, gen) = (self.tx.clone(), self.gen);
            if let Err(e) = conn
                .reducers
                .register_for_tournament_then(tid, move |_, o| {
                    let _ = tx.send(Msg {
                        gen,
                        ev: Ev::RegisterDone {
                            tournament_id: tid,
                            result: flatten_outcome(o),
                        },
                    });
                })
            {
                warn!("register_for_tournament send failed: {e}");
            }
        }
    }

    // ------------------------------------------------------------- tick

    fn tick(&mut self) -> Result<()> {
        let now = Instant::now();
        if self.lag_reconnect_at.is_some_and(|t| now >= t) {
            self.lag_reconnect_at = None;
            if self.state == ConnState::Ready {
                self.force_reconnect();
            }
        }
        if self.state == ConnState::Down && now >= self.reconnect_at {
            if self.failures > self.opts.max_reconnect_failures {
                bail!(
                    "giving up after {} consecutive connection failures",
                    self.failures
                );
            }
            self.connect();
        }
        if self.state != ConnState::Down && self.conn.as_ref().is_some_and(|c| !c.is_active()) {
            self.reconnects += 1;
            self.schedule_reconnect("connection went inactive".into());
        }
        if self.state == ConnState::Connecting
            && now.duration_since(self.connecting_since) > Duration::from_secs(45)
        {
            self.schedule_reconnect("timed out waiting for the subscription".into());
        }
        if self.poke && now.duration_since(self.last_poke) >= Duration::from_millis(500) {
            self.poke = false;
            self.last_poke = now;
            self.autopilot();
        }
        if now.duration_since(self.last_watchdog) >= Duration::from_secs(5) {
            self.last_watchdog = now;
            self.watchdog();
        }
        if now.duration_since(self.last_heartbeat) >= Duration::from_secs(10) {
            let state = match self.state {
                ConnState::Ready => "connected",
                ConnState::Connecting => "connecting",
                ConnState::Down => "disconnected",
            };
            self.write_heartbeat(state);
        }
        Ok(())
    }

    /// Catch anything a missed event would have left behind.
    fn watchdog(&mut self) {
        if self.state != ConnState::Ready {
            return;
        }
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let mut ended = Vec::new();
        let mut missed = Vec::new();
        for (id, ctx) in &self.matches {
            match conn.db.match_state().id().find(id) {
                Some(m) if matches!(m.status, MatchStatus::Running) => {
                    if let Some(a) = m.current_auction_id.filter(|&a| a > ctx.last_open) {
                        // Give the event queue 250 ms before calling it missed.
                        let age =
                            |r: &Auction| now_ms() - r.opens_at.to_micros_since_unix_epoch() / 1000;
                        if let Some(row) = conn.db.auction().id().find(&a).filter(|r| age(r) > 250)
                        {
                            missed.push((row, m.bag_total));
                        }
                    }
                }
                _ => ended.push(*id),
            }
        }
        for (a, bag) in missed {
            warn!("m{}: caught up on missed auction {}", a.match_id, a.id);
            self.on_open_event(
                a.match_id,
                a.id,
                a.letter.clone(),
                Some(bag),
                a.closes_at.to_micros_since_unix_epoch(),
            );
        }
        for id in ended {
            self.finalize(id);
        }
        // New matches we never heard about.
        if let Some(conn) = self.conn.as_ref() {
            let fresh: Vec<u64> = conn
                .db
                .match_state()
                .iter()
                .filter(|m| matches!(m.status, MatchStatus::Running))
                .map(|m| m.id)
                .filter(|id| {
                    !self.matches.contains_key(id)
                        && !self.ignored.contains(id)
                        && !self.finished.contains(id)
                })
                .collect();
            for id in fresh {
                self.ensure_ctx(id);
            }
        }
        self.autopilot();
        // Keep the history cache bounded on long runs.
        if self.phase2_next.is_none() && self.floor_checked.elapsed() >= Duration::from_secs(600) {
            self.floor_checked = Instant::now();
            if let Some(conn) = self.conn.as_ref() {
                let floor = Self::compute_floor(&conn.db);
                if floor > self.floor + 25 {
                    if let Some(h) = self.subscribe_phase2(floor, Ev::FloorApplied) {
                        self.phase2_next = Some((floor, h));
                    }
                }
            }
        }
    }

    fn write_heartbeat(&mut self, state: &str) {
        self.last_heartbeat = Instant::now();
        let mut live: Vec<u64> = self.matches.keys().copied().collect();
        live.sort_unstable();
        self.tel.heartbeat(&Heartbeat {
            ts_ms: now_ms(),
            pid: std::process::id(),
            mode: self.opts.mode.name().into(),
            state: state.into(),
            bot_id: self.my_bot,
            matches_live: live,
            matches_finished: self.matches_finished,
            last_event_age_ms: self.last_event.elapsed().as_millis() as i64,
            uptime_s: self.started.elapsed().as_secs(),
            reconnects: self.reconnects,
        });
    }
}

/// Rebuild a match from the cache: everything public that happened so far,
/// in server order. Returns the context and the open auction to act on.
fn build_ctx(env: &Env, common: &Common, m: &Match, me: u64) -> (MatchCtx, Option<Auction>) {
    let db = &env.conn.db;
    let parts = participants(db, m.id);
    let ids: Vec<u64> = parts.iter().map(|p| p.bot_id).collect();
    let (kind, kind_name) = kind_of(&m.auction_type);
    let srs = m.starting_rack_size as usize;
    let tournament_id = db
        .tournament_match()
        .iter()
        .find(|t| t.match_id == m.id)
        .map(|t| t.tournament_id);
    let mut params = common.params();
    let mut arm = "A".to_string();
    // Live A/B: casual matches only; tournaments always get the champion.
    if tournament_id.is_none() {
        if let Some((p, label)) = common
            .ab()
            .and_then(|ab| crate::config::ab_params(&common.config_dir, &ab, m.id))
        {
            params = p;
            arm = label;
        }
    }
    let duel = common.duel_params();
    let field = common.field();
    let duel_loaded = duel.is_some();
    let duel_active = duel_loaded && ids.len() <= 2;
    let active = if duel_active {
        duel.clone().unwrap_or_else(|| params.clone())
    } else {
        params.clone()
    };
    let params_json = serde_json::to_value(&active).unwrap_or(serde_json::Value::Null);
    let mut brain = Brain::new(params, field, m.id ^ 0x5eed_cafe).with_duel(duel);
    brain.start_match();
    let mut tracker = Tracker::new(m.id, me, kind, &ids, srs);
    brain.sync_profile(&tracker);

    let mut auctions: Vec<Auction> = db.auction().iter().filter(|a| a.match_id == m.id).collect();
    auctions.sort_by_key(|a| a.id);
    let open = auctions
        .iter()
        .rev()
        .find(|a| matches!(a.status, AuctionStatus::Open))
        .cloned();
    let results: Vec<AuctionResult> = db
        .auction_result()
        .iter()
        .filter(|r| r.match_id == m.id)
        .collect();
    let mut words: Vec<WordPlay> = db
        .word_play()
        .iter()
        .filter(|w| w.match_id == m.id)
        .collect();
    words.sort_by_key(|w| w.id);

    if env.mode.is_live() {
        // Before any auction the rack is the deal; reconstruct it if we join late.
        let won = results.iter().filter(|r| r.winner_bot_id == Some(me)).fold(
            Rack::EMPTY,
            |mut acc, r| {
                if let Some(l) = letter_of(&r.letter) {
                    acc.add(l);
                }
                acc
            },
        );
        let mine = words
            .iter()
            .filter(|w| w.bot_id == me)
            .map(|w| w.word.as_str());
        tracker.set_my_rack(derive_deal(cache_rack(db, m.id, me), mine, &won));
    }

    let mut ctx = MatchCtx {
        match_id: m.id,
        me,
        arm: arm.clone(),
        kind_name: kind_name.to_string(),
        tracker,
        brain,
        tournament_id,
        last_open: 0,
        last_result: 0,
        last_word: 0,
        open: None,
        pending: Vec::new(),
        recs: HashMap::new(),
        resumed: false,
        letters_won: 0,
        spent: 0,
        words_played: 0,
        phantom: Vec::new(),
    };
    if !env.mode.is_live() {
        ctx.phantom = phantom_deal(&ctx.tracker.unseen(), srs, m.id);
    }

    let open_id = open.as_ref().map(|a| a.id);
    let mut hist: Vec<HistItem> = Vec::new();
    for a in auctions.iter().filter(|a| Some(a.id) != open_id) {
        hist.push(HistItem::Open {
            auction_id: a.id,
            letter: a.letter.clone(),
            ts: a.opens_at.to_micros_since_unix_epoch(),
        });
    }
    for r in &results {
        hist.push(HistItem::Result {
            auction_id: r.auction_id,
            letter: r.letter.clone(),
            winner: r.winner_bot_id,
            paid: r.paid,
            ts: r.closed_at.to_micros_since_unix_epoch(),
        });
    }
    for w in &words {
        hist.push(HistItem::Word {
            id: w.id,
            bot_id: w.bot_id,
            word: w.word.clone(),
            reward: w.total_reward,
            ts: w.played_at.to_micros_since_unix_epoch(),
        });
    }
    order_history(&mut hist);
    let n = ids.len();
    for item in &hist {
        match item {
            HistItem::Open { auction_id, .. } => {
                feed_tracker(&mut ctx.tracker, item, n, srs);
                ctx.last_open = *auction_id;
            }
            // These also feed the brain's price model and our counters.
            HistItem::Result {
                auction_id,
                letter,
                winner,
                paid,
                ..
            } => ctx.apply_result(env, *auction_id, letter, *winner, *paid),
            HistItem::Word {
                id, bot_id, word, ..
            } => ctx.apply_word(*id, *bot_id, word),
        }
    }
    ctx.resumed = ctx.tracker.auctions_seen > 0;
    ctx.refresh(env);
    env.tel.match_line(
        m.id,
        &MatchLine::Start {
            v: SCHEMA_VERSION,
            ts_ms: now_ms(),
            mode: env.mode.name().into(),
            match_id: m.id,
            my_bot_id: me,
            auction_type: kind_name.into(),
            n_players: n,
            participants: ids,
            starting_rack_size: m.starting_rack_size,
            rack: ctx.tracker.my_rack.to_string(),
            resumed: ctx.resumed,
            auctions_before_join: ctx.tracker.auctions_seen,
            tournament_id,
            duel_loaded,
            duel_active,
            arm,
            params: params_json,
        },
    );
    (ctx, open)
}

/// Draw `k` letters from `pool` with a seeded xorshift (spectator stand-ins).
fn phantom_deal(pool: &Rack, k: usize, seed: u64) -> Vec<vickrey_core::Letter> {
    let mut bag: Vec<vickrey_core::Letter> = pool.letters().collect();
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(k);
    for _ in 0..k.min(bag.len()) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let i = (x % bag.len() as u64) as usize;
        out.push(bag.swap_remove(i));
    }
    out
}

/// Touch the solver and valuation paths once so the first real auction
/// doesn't pay for cold caches.
fn prewarm(common: &Common) {
    let t = Instant::now();
    let ids = [1, 2, 3, 4, 5, 6];
    let mut tracker = Tracker::new(0, 1, AuctionKind::Vickrey, &ids, 5);
    tracker.set_my_rack(Rack::from_word("AEINR"));
    let mut brain = Brain::new(common.params(), common.field(), 7).with_duel(common.duel_params());
    brain.start_match();
    for (i, c) in "ETAOSL".chars().enumerate() {
        if let Some(l) = vickrey_core::index(c) {
            tracker.on_auction_open(l, 60 - i as u32);
            let _ = brain.plays(&tracker);
            let _ = brain.bid(&tracker, l);
            tracker.on_auction_result(l, Some(2), 5);
        }
    }
    let _ = brain.final_plays(&Rack::from_word("AEINRSTLOQUZ"));
    info!("brain prewarmed in {:.0} ms", ms(t));
}

pub struct RunStats {
    pub latencies_ms: Vec<f64>,
    pub matches_finished: u64,
}

impl RunStats {
    pub fn report(&self) -> String {
        let p = |q| {
            percentile(&self.latencies_ms, q)
                .map(|x| format!("{x:.1}"))
                .unwrap_or_else(|| "-".into())
        };
        let max = self.latencies_ms.iter().cloned().fold(f64::NAN, f64::max);
        format!(
            "decisions {} | latency ms p50 {} p95 {} p99 {} max {} | matches finished {}",
            self.latencies_ms.len(),
            p(50.0),
            p(95.0),
            p(99.0),
            if max.is_nan() {
                "-".into()
            } else {
                format!("{max:.1}")
            },
            self.matches_finished
        )
    }
}

#[cfg(test)]
mod dump_tests {
    use super::dump_bag_for;
    use std::collections::VecDeque;

    #[test]
    fn dump_zone_tracks_latency() {
        assert_eq!(dump_bag_for(&VecDeque::new()), 0);
        assert_eq!(dump_bag_for(&VecDeque::from(vec![90.0; 20])), 0);
        assert_eq!(dump_bag_for(&VecDeque::from(vec![600.0; 20])), 1);
        assert_eq!(dump_bag_for(&VecDeque::from(vec![1800.0; 20])), 4);
        assert_eq!(dump_bag_for(&VecDeque::from(vec![4300.0; 20])), 9);
        assert_eq!(dump_bag_for(&VecDeque::from(vec![60_000.0; 20])), 12);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_parses() {
        assert_eq!("all".parse::<Shadow>(), Ok(Shadow::All));
        assert_eq!("ALL".parse::<Shadow>(), Ok(Shadow::All));
        assert_eq!("42".parse::<Shadow>(), Ok(Shadow::Bot(42)));
        assert!("x".parse::<Shadow>().is_err());
    }

    #[test]
    fn auction_kinds_map() {
        assert_eq!(kind_of(&AuctionType::Vickrey).0, AuctionKind::Vickrey);
        assert_eq!(kind_of(&AuctionType::FirstPrice).1, "FirstPrice");
    }

    #[test]
    fn stats_report_handles_empty() {
        let s = RunStats {
            latencies_ms: vec![],
            matches_finished: 0,
        };
        assert!(s.report().contains("p50 -"));
        let s = RunStats {
            latencies_ms: vec![1.0, 2.0, 3.0],
            matches_finished: 1,
        };
        assert!(s.report().contains("p50 2.0"));
    }
}
