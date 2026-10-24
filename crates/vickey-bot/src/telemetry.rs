//! Append-only JSONL telemetry. Field names are a contract with the tuner;
//! see `TELEMETRY.md` before renaming anything.

use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Bumped when a field changes meaning or disappears.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct BidRec {
    pub amount: i64,
    pub value: f64,
    pub lambda: f64,
    pub p_win: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PlayerRec {
    pub bot_id: u64,
    pub name: String,
    pub score: i64,
    pub balance: i64,
}

/// One line of `matches/<match_id>.jsonl`.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MatchLine {
    Start {
        v: u32,
        ts_ms: i64,
        mode: String,
        match_id: u64,
        my_bot_id: u64,
        auction_type: String,
        n_players: usize,
        participants: Vec<u64>,
        starting_rack_size: u32,
        rack: String,
        resumed: bool,
        auctions_before_join: u32,
        tournament_id: Option<u64>,
        /// `params_duel.json` was loaded for this match.
        duel_loaded: bool,
        /// The heads-up profile is in force (loaded and at most 2 players).
        duel_active: bool,
        params: serde_json::Value,
    },
    Auction {
        v: u32,
        ts_ms: i64,
        mode: String,
        match_id: u64,
        auction_id: u64,
        seq: u32,
        letter: String,
        bag_total: u32,
        last: bool,
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
        fast: bool,
        rebid: bool,
        words: Vec<String>,
        winner: Option<u64>,
        we_won: bool,
        paid: i64,
        top_bid: Option<i64>,
        balance_after: i64,
    },
    Word {
        v: u32,
        ts_ms: i64,
        mode: String,
        match_id: u64,
        auction_id: u64,
        word: String,
        phase: String,
        expected_reward: u32,
        ok: bool,
        error: Option<String>,
        rtt_ms: f64,
    },
    End {
        v: u32,
        ts_ms: i64,
        mode: String,
        match_id: u64,
        my_bot_id: u64,
        auction_type: String,
        n_players: usize,
        place: usize,
        our_score: i64,
        our_balance: i64,
        winner_score: i64,
        letters_won: u32,
        spent: i64,
        words_played: u32,
        auctions_seen: u32,
        resumed: bool,
        tournament_id: Option<u64>,
        participants: Vec<PlayerRec>,
    },
}

/// One line of `summary.jsonl` per finished match.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SummaryLine {
    pub v: u32,
    pub ts: i64,
    pub mode: String,
    pub match_id: u64,
    pub my_bot_id: u64,
    pub n_players: usize,
    pub auction_type: String,
    pub our_score: i64,
    pub our_balance: i64,
    pub place: usize,
    pub winner_score: i64,
    /// Every participant's final score, highest first.
    pub scores: Vec<i64>,
    pub letters_won: u32,
    pub spent: i64,
    pub words_played: u32,
    pub resumed: bool,
    pub tournament_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Heartbeat {
    pub ts_ms: i64,
    pub pid: u32,
    pub mode: String,
    pub state: String,
    pub bot_id: Option<u64>,
    pub matches_live: Vec<u64>,
    pub matches_finished: u64,
    pub last_event_age_ms: i64,
    pub uptime_s: u64,
    pub reconnects: u64,
}

/// Writes telemetry under one root (`runs/` live, `runs/spectate/` when shadowing).
#[derive(Clone, Debug)]
pub struct Telemetry {
    root: PathBuf,
}

impl Telemetry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        if let Err(e) = std::fs::create_dir_all(root.join("matches")) {
            tracing::warn!("telemetry dir {}: {e}", root.display());
        }
        Telemetry { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn match_path(&self, match_id: u64) -> PathBuf {
        self.root.join("matches").join(format!("{match_id}.jsonl"))
    }

    pub fn match_line(&self, match_id: u64, line: &MatchLine) {
        append_json(&self.match_path(match_id), line);
    }

    pub fn summary(&self, line: &SummaryLine) {
        append_json(&self.root.join("summary.jsonl"), line);
    }

    /// Replaces `heartbeat` atomically so readers never see half a file.
    pub fn heartbeat(&self, hb: &Heartbeat) {
        let path = self.root.join("heartbeat");
        let tmp = self.root.join("heartbeat.tmp");
        let body = match serde_json::to_string(hb) {
            Ok(s) => s + "\n",
            Err(_) => return,
        };
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

fn append_json<T: Serialize>(path: &Path, value: &T) {
    let line = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("telemetry encode: {e}");
            return;
        }
    };
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
    if let Err(e) = res {
        tracing::warn!("telemetry write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_lines_are_tagged_and_stable() {
        let line = MatchLine::Word {
            v: SCHEMA_VERSION,
            ts_ms: 1,
            mode: "live".into(),
            match_id: 9,
            auction_id: 77,
            word: "QUIZ".into(),
            phase: "final".into(),
            expected_reward: 33,
            ok: true,
            error: None,
            rtt_ms: 12.5,
        };
        let v: serde_json::Value = serde_json::to_value(&line).unwrap();
        assert_eq!(v["type"], "word");
        assert_eq!(v["phase"], "final");
        assert_eq!(v["expected_reward"], 33);
        assert!(v["error"].is_null());

        let a = MatchLine::Auction {
            v: 1,
            ts_ms: 2,
            mode: "spectate".into(),
            match_id: 9,
            auction_id: 78,
            seq: 3,
            letter: "E".into(),
            bag_total: 64,
            last: false,
            rack: "AEI".into(),
            balance: 90,
            score: 0,
            bid: Some(BidRec { amount: 5, value: 2.0, lambda: 0.4, p_win: 0.3 }),
            submitted: false,
            bid_error: None,
            decision_ms: 3.2,
            plays_ms: 1.0,
            bid_ms: 2.2,
            ms_left: Some(900),
            fast: false,
            rebid: false,
            words: vec![],
            winner: Some(4),
            we_won: false,
            paid: 6,
            top_bid: Some(8),
            balance_after: 90,
        };
        let v: serde_json::Value = serde_json::to_value(&a).unwrap();
        assert_eq!(v["type"], "auction");
        assert_eq!(v["bid"]["amount"], 5);
        assert_eq!(v["winner"], 4);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        for k in ["letter", "bag_total", "decision_ms", "paid", "balance_after", "top_bid"] {
            assert!(keys.contains(&k), "missing {k}");
        }
    }

    #[test]
    fn writes_append_lines() {
        let dir = std::env::temp_dir().join(format!("vickey-tel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Telemetry::new(&dir);
        let s = SummaryLine {
            v: 1,
            ts: 5,
            mode: "live".into(),
            match_id: 3,
            my_bot_id: 1,
            n_players: 2,
            auction_type: "Vickrey".into(),
            our_score: 10,
            our_balance: 4,
            place: 1,
            winner_score: 10,
            scores: vec![10, 3],
            letters_won: 2,
            spent: 7,
            words_played: 1,
            resumed: false,
            tournament_id: None,
        };
        t.summary(&s);
        t.summary(&s);
        let text = std::fs::read_to_string(dir.join("summary.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
        let back: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(back["scores"][1], 3);
        t.heartbeat(&Heartbeat {
            ts_ms: 1,
            pid: 1,
            mode: "live".into(),
            state: "connected".into(),
            bot_id: Some(1),
            matches_live: vec![],
            matches_finished: 0,
            last_event_age_ms: 0,
            uptime_s: 0,
            reconnects: 0,
        });
        assert!(dir.join("heartbeat").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
