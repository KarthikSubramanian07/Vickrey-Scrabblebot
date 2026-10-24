//! Everything a bot can know about one match, folded from public events.
//!
//! Shared by the simulator and the live client so both feed the brain the
//! exact same view. Opponents' racks are reconstructed from auction wins
//! and word plays; letters they play beyond what they won must have come
//! from their hidden starting tiles, which lets us shrink the unseen pool.

use vickey_core::letters::{BAG, STARTING_BALANCE};
use vickey_core::{index, Letter, Rack, ALPHABET};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AuctionKind {
    #[default]
    Vickrey,
    FirstPrice,
}

#[derive(Clone, Debug)]
pub struct Player {
    pub bot_id: u64,
    pub balance: i64,
    pub score: i64,
    /// Letters won at auction.
    pub won: Rack,
    /// Letters spent in played words.
    pub played: Rack,
    /// Hidden starting tiles we have deduced (played beyond what was won).
    pub start_known: Rack,
    pub starting_rack: usize,
}

impl Player {
    fn new(bot_id: u64, starting_rack: usize) -> Self {
        Player {
            bot_id,
            balance: STARTING_BALANCE,
            score: 0,
            won: Rack::EMPTY,
            played: Rack::EMPTY,
            start_known: Rack::EMPTY,
            starting_rack,
        }
    }

    /// Letters we know this player still holds (excludes undeduced hidden tiles).
    pub fn known_rack(&self) -> Rack {
        let mut r = self.won;
        r.plus(&self.start_known);
        for l in 0..ALPHABET {
            r.0[l] = r.0[l].saturating_sub(self.played.0[l]);
        }
        r
    }

    /// Hidden starting tiles not yet revealed.
    pub fn hidden_left(&self) -> usize {
        self.starting_rack.saturating_sub(self.start_known.len())
    }
}

#[derive(Clone, Debug)]
pub struct Tracker {
    pub match_id: u64,
    pub my_id: u64,
    pub kind: AuctionKind,
    pub players: Vec<Player>,
    /// Our rack as the server reports it.
    pub my_rack: Rack,
    /// Our dealt tiles (first rack snapshot).
    pub my_start: Rack,
    /// Every letter put up for auction so far, including an open one.
    pub revealed: Rack,
    /// `match_state.bag_total`: letters still in the bag after the open auction's draw.
    pub bag_left: u32,
    pub total_auctions: u32,
    pub auctions_seen: u32,
    /// Per-letter observed clearing prices this match (`paid` when someone won).
    pub paid_log: Vec<(Letter, i64, u32)>,
}

impl Tracker {
    pub fn new(
        match_id: u64,
        my_id: u64,
        kind: AuctionKind,
        bot_ids: &[u64],
        starting_rack: usize,
    ) -> Self {
        let n = bot_ids.len() as u32;
        let total = 98u32.saturating_sub(n * starting_rack as u32);
        Tracker {
            match_id,
            my_id,
            kind,
            players: bot_ids
                .iter()
                .map(|&b| Player::new(b, starting_rack))
                .collect(),
            my_rack: Rack::EMPTY,
            my_start: Rack::EMPTY,
            revealed: Rack::EMPTY,
            bag_left: total,
            total_auctions: total,
            auctions_seen: 0,
            paid_log: Vec::new(),
        }
    }

    pub fn n_players(&self) -> usize {
        self.players.len()
    }

    pub fn me(&self) -> Option<&Player> {
        self.players.iter().find(|p| p.bot_id == self.my_id)
    }

    pub fn my_balance(&self) -> i64 {
        self.me().map(|p| p.balance).unwrap_or(0)
    }

    pub fn player_mut(&mut self, bot_id: u64) -> Option<&mut Player> {
        self.players.iter_mut().find(|p| p.bot_id == bot_id)
    }

    pub fn opponents(&self) -> impl Iterator<Item = &Player> {
        let me = self.my_id;
        self.players.iter().filter(move |p| p.bot_id != me)
    }

    /// Set our rack from the server. The first non-empty snapshot is our deal.
    pub fn set_my_rack(&mut self, rack: Rack) {
        if self.my_start.is_empty() && self.auctions_seen <= 1 && !rack.is_empty() {
            // Before any auction closes the rack is exactly the deal.
            let won_me = self.me().map(|p| p.won).unwrap_or(Rack::EMPTY);
            if won_me.is_empty() {
                self.my_start = rack;
            }
        }
        self.my_rack = rack;
    }

    /// A new auction opened for `letter`; `bag_left` is `match_state.bag_total`.
    pub fn on_auction_open(&mut self, letter: Letter, bag_left: u32) {
        self.revealed.add(letter);
        self.auctions_seen += 1;
        self.bag_left = bag_left;
    }

    pub fn on_auction_result(&mut self, letter: Letter, winner: Option<u64>, paid: i64) {
        if let Some(w) = winner {
            if let Some(p) = self.player_mut(w) {
                p.won.add(letter);
            }
            self.paid_log.push((letter, paid, self.auctions_seen));
        }
    }

    pub fn on_word(&mut self, bot_id: u64, word: &str) {
        let Some(p) = self.player_mut(bot_id) else {
            return;
        };
        for c in word.chars() {
            if let Some(l) = index(c) {
                p.played.add(l);
                // More of `l` played than won and deduced so far: a hidden tile.
                let have = p.won.0[l] as u32 + p.start_known.0[l] as u32;
                if (p.played.0[l] as u32) > have && p.start_known.len() < p.starting_rack {
                    p.start_known.add(l);
                }
            }
        }
    }

    pub fn on_participant(&mut self, bot_id: u64, balance: i64, score: i64) {
        if let Some(p) = self.player_mut(bot_id) {
            p.balance = balance;
            p.score = score;
        }
    }

    /// Letters whose location we don't know: still in the bag, or hidden in
    /// an opponent's undisclosed starting tiles.
    pub fn unseen(&self) -> Rack {
        let mut u = Rack(BAG);
        for l in 0..ALPHABET {
            let mut gone = self.revealed.0[l] as u32 + self.my_start.0[l] as u32;
            for p in self.opponents() {
                gone += p.start_known.0[l] as u32;
            }
            u.0[l] = (u.0[l] as u32).saturating_sub(gone) as u8;
        }
        u
    }

    /// Whether the open auction is the last one (its letter can never be played).
    pub fn is_last_auction(&self) -> bool {
        self.bag_left == 0
    }

    /// Fraction of the match's auctions still to come, in `[0, 1]`.
    pub fn progress(&self) -> f64 {
        if self.total_auctions == 0 {
            return 1.0;
        }
        (self.auctions_seen as f64 / self.total_auctions as f64).clamp(0.0, 1.0)
    }

    pub fn max_opponent_balance(&self) -> i64 {
        self.opponents().map(|p| p.balance).max().unwrap_or(0)
    }

    /// Our current place (1 = leading) by score, ties sharing the higher place.
    pub fn my_place(&self) -> usize {
        let mine = self.me().map(|p| p.score).unwrap_or(0);
        1 + self.opponents().filter(|p| p.score > mine).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduces_hidden_tiles() {
        let mut t = Tracker::new(1, 1, AuctionKind::Vickrey, &[1, 2], 5);
        t.on_auction_open(index('A').unwrap(), 87);
        t.on_auction_result(index('A').unwrap(), Some(2), 5);
        t.on_word(2, "AXE"); // A was won; X and E must be hidden tiles
        let p = &t.players[1];
        assert_eq!(p.start_known, Rack::from_word("XE"));
        assert_eq!(p.hidden_left(), 3);
        let u = t.unseen();
        assert_eq!(u.0[index('X').unwrap()], 0);
        assert_eq!(u.0[index('A').unwrap()], 8);
    }
}
