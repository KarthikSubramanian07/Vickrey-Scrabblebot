//! One match, played by the server's rules (`game.ts`):
//!
//! * 98-tile bag, `starting_rack` tiles dealt to each seat, then one
//!   sealed-bid auction per remaining tile until the bag is empty.
//! * Highest positive bid wins, ties broken at random. Vickrey pays the
//!   runner-up's bid (or the reserve of 1 for a lone bidder); first-price
//!   pays the bid. A winner who can't cover the price gets nothing.
//! * Tiles nobody bids on are discarded.
//! * Server sims play their longest 3+ word inside the close, so they can
//!   use the last tile. Client bots act while the next auction is open, so
//!   the match ends before they can use the last tile.

use crate::agents::{server_sim_play, Agent};
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use vickrey_brain::{AuctionKind, Tracker};
use vickrey_core::letters::{BAG, STARTING_BALANCE, STARTING_RACK};
use vickrey_core::{dictionary, word_reward, Letter, Rack};

#[derive(Clone, Debug)]
pub struct Seat {
    pub name: String,
    pub score: i64,
    pub balance: i64,
    pub letters_won: u32,
    pub spent: i64,
    pub words: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MatchResult {
    pub seats: Vec<Seat>,
}

impl MatchResult {
    /// 1-based place of seat `i`; ties share the higher place.
    pub fn place(&self, i: usize) -> usize {
        let s = self.seats[i].score;
        1 + self.seats.iter().filter(|x| x.score > s).count()
    }
}

#[derive(Clone, Debug)]
pub struct MatchConfig {
    pub kind: AuctionKind,
    pub starting_rack: usize,
}

impl Default for MatchConfig {
    fn default() -> Self {
        MatchConfig {
            kind: AuctionKind::Vickrey,
            starting_rack: STARTING_RACK,
        }
    }
}

struct Live {
    rack: Rack,
    balance: i64,
    score: i64,
    won: u32,
    spent: i64,
    words: Vec<String>,
}

fn draw(bag: &mut Vec<Letter>, rng: &mut SmallRng) -> Option<Letter> {
    if bag.is_empty() {
        return None;
    }
    let i = rng.gen_range(0..bag.len());
    Some(bag.swap_remove(i))
}

/// Play `word` for seat `i` if it is valid and fits; returns whether it scored.
fn try_play(live: &mut Live, word: &str) -> bool {
    let d = dictionary();
    if !d.is_valid(word) {
        return false;
    }
    let need = Rack::from_word(word);
    if !live.rack.contains(&need) {
        return false;
    }
    live.rack.sub(&need);
    let r = word_reward(word) as i64;
    live.balance += r;
    live.score += r;
    live.words.push(word.to_string());
    true
}

pub fn play_match(agents: &mut [Box<dyn Agent>], cfg: &MatchConfig, seed: u64) -> MatchResult {
    let mut rng = SmallRng::seed_from_u64(seed);
    let n = agents.len();
    let ids: Vec<u64> = (1..=n as u64).collect();
    let mut bag: Vec<Letter> = (0..26)
        .flat_map(|l| std::iter::repeat_n(l, BAG[l] as usize))
        .collect();
    let mut live: Vec<Live> = (0..n)
        .map(|_| Live {
            rack: Rack::EMPTY,
            balance: STARTING_BALANCE,
            score: 0,
            won: 0,
            spent: 0,
            words: Vec::new(),
        })
        .collect();
    for seat in live.iter_mut() {
        for _ in 0..cfg.starting_rack {
            if let Some(l) = draw(&mut bag, &mut rng) {
                seat.rack.add(l);
            }
        }
    }
    let mut trackers: Vec<Tracker> = ids
        .iter()
        .map(|&me| Tracker::new(seed, me, cfg.kind, &ids, cfg.starting_rack))
        .collect();
    for i in 0..n {
        trackers[i].set_my_rack(live[i].rack);
        agents[i].start(&trackers[i]);
    }

    let sync = |trackers: &mut Vec<Tracker>, live: &Vec<Live>| {
        for t in trackers.iter_mut() {
            for (j, l) in live.iter().enumerate() {
                t.on_participant(ids[j], l.balance, l.score);
            }
        }
    };
    let announce_word = |trackers: &mut Vec<Tracker>, who: usize, w: &str| {
        for t in trackers.iter_mut() {
            t.on_word(ids[who], w);
        }
    };

    while let Some(letter) = draw(&mut bag, &mut rng) {
        let bag_left = bag.len() as u32;
        for t in trackers.iter_mut() {
            t.on_auction_open(letter, bag_left);
        }
        sync(&mut trackers, &live);

        // Bots react inside the open window: words first (coins land
        // before the bid is checked), then sealed bids.
        for i in 0..n {
            if agents[i].is_server_sim() {
                continue;
            }
            trackers[i].set_my_rack(live[i].rack);
            let words = if bag_left == 0 {
                agents[i].final_plays(&trackers[i])
            } else {
                agents[i].plays(&trackers[i])
            };
            for w in words {
                if try_play(&mut live[i], &w) {
                    announce_word(&mut trackers, i, &w);
                }
            }
            trackers[i].set_my_rack(live[i].rack);
        }
        sync(&mut trackers, &live);

        let mut bids: Vec<(usize, i64)> = Vec::with_capacity(n);
        for i in 0..n {
            let b = agents[i]
                .bid(&trackers[i], letter)
                .min(live[i].balance)
                .max(0);
            if b > 0 {
                bids.push((i, b));
            }
        }
        bids.shuffle(&mut rng);
        bids.sort_by_key(|b| std::cmp::Reverse(b.1)); // stable: shuffled ties stay random
        let mut winner = None;
        let mut paid = 0;
        if let Some(&(w, top)) = bids.first() {
            paid = match cfg.kind {
                AuctionKind::FirstPrice => top,
                AuctionKind::Vickrey => bids.get(1).map(|b| b.1).unwrap_or(top.min(1)),
            };
            if live[w].balance >= paid {
                live[w].balance -= paid;
                live[w].spent += paid;
                live[w].rack.add(letter);
                live[w].won += 1;
                winner = Some(w);
            }
        }
        let winner_id = winner.map(|w| ids[w]);
        for i in 0..n {
            trackers[i].on_auction_result(letter, winner_id, paid);
        }
        if let Some(w) = winner {
            if agents[w].is_server_sim() {
                if let Some(word) = server_sim_play(&live[w].rack) {
                    if try_play(&mut live[w], &word) {
                        announce_word(&mut trackers, w, &word);
                    }
                }
            }
        }
        sync(&mut trackers, &live);
        for i in 0..n {
            trackers[i].set_my_rack(live[i].rack);
            agents[i].observe(&trackers[i], letter, winner_id, paid);
        }
    }

    MatchResult {
        seats: (0..n)
            .map(|i| Seat {
                name: agents[i].name(),
                score: live[i].score,
                balance: live[i].balance,
                letters_won: live[i].won,
                spent: live[i].spent,
                words: live[i].words.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{FieldBot, ServerSim, SimStrategy, Starter};

    #[test]
    fn six_sims_play_68_auctions_and_score() {
        let mut agents: Vec<Box<dyn Agent>> = (0..6)
            .map(|i| {
                let s = [
                    SimStrategy::Cheapskate,
                    SimStrategy::ValueBidder,
                    SimStrategy::Aggressive,
                ][i % 3];
                Box::new(ServerSim::new(s, &format!("sim{i}"), i as u64)) as Box<dyn Agent>
            })
            .collect();
        let r = play_match(&mut agents, &MatchConfig::default(), 42);
        let won: u32 = r.seats.iter().map(|s| s.letters_won).sum();
        assert!(won <= 68);
        assert!(r.seats.iter().any(|s| s.score > 0));
        // Aggressive sims dominate sim-only tables, as on the live server.
        let agg = r.seats[2].score + r.seats[5].score;
        let cheap = r.seats[0].score + r.seats[3].score;
        assert!(agg >= cheap);
    }

    #[test]
    fn mixed_table_runs() {
        let mut agents: Vec<Box<dyn Agent>> = vec![
            Box::new(Starter::new("starter")),
            Box::new(FieldBot::preset("shark", 1)),
            Box::new(FieldBot::preset("grinder", 2)),
            Box::new(FieldBot::preset("spender", 3)),
            Box::new(FieldBot::preset("nibbler", 4)),
            Box::new(ServerSim::new(SimStrategy::Aggressive, "Brutus", 5)),
        ];
        let r = play_match(&mut agents, &MatchConfig::default(), 7);
        for s in &r.seats {
            assert!(s.balance >= 0);
            for w in &s.words {
                assert!(dictionary().is_valid(w));
            }
        }
    }
}
