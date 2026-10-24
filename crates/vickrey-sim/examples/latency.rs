//! Per-decision latency of the brain inside simulated matches.
//! `cargo run --release -p vickrey-sim --example latency -- [players]`
use std::sync::{Arc, Mutex};
use std::time::Instant;
use vickrey_brain::{FieldModel, Params, Tracker};
use vickrey_core::Letter;
use vickrey_sim::agents::{Agent, FieldBot, Vickrey};
use vickrey_sim::engine::{play_match, MatchConfig};

type Log = Arc<Mutex<(Vec<f64>, Vec<f64>)>>;

struct Timed(Vickrey, Log);
impl Agent for Timed {
    fn name(&self) -> String {
        "timed".into()
    }
    fn start(&mut self, t: &Tracker) {
        self.0.start(t)
    }
    fn bid(&mut self, t: &Tracker, l: Letter) -> i64 {
        let s = Instant::now();
        let b = self.0.bid(t, l);
        self.1
            .lock()
            .unwrap()
            .0
            .push(s.elapsed().as_secs_f64() * 1e3);
        b
    }
    fn observe(&mut self, t: &Tracker, l: Letter, w: Option<u64>, p: i64) {
        self.0.observe(t, l, w, p)
    }
    fn plays(&mut self, t: &Tracker) -> Vec<String> {
        let s = Instant::now();
        let w = self.0.plays(t);
        self.1
            .lock()
            .unwrap()
            .1
            .push(s.elapsed().as_secs_f64() * 1e3);
        w
    }
    fn final_plays(&mut self, t: &Tracker) -> Vec<String> {
        let s = Instant::now();
        let w = self.0.final_plays(t);
        self.1
            .lock()
            .unwrap()
            .1
            .push(s.elapsed().as_secs_f64() * 1e3);
        w
    }
}

fn q(v: &[f64], p: f64) -> f64 {
    v[((v.len() - 1) as f64 * p) as usize]
}

fn main() {
    let players: usize = std::env::args()
        .nth(1)
        .and_then(|x| x.parse().ok())
        .unwrap_or(6);
    let p = Params::load(std::path::Path::new(
        &std::env::var("LAT_PARAMS").unwrap_or("config/params.json".into()),
    ))
    .unwrap_or_default();
    let log: Log = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    for seed in 0..4 {
        let mut agents: Vec<Box<dyn Agent>> = vec![Box::new(Timed(
            Vickrey::new("v", p.clone(), FieldModel::default(), seed),
            log.clone(),
        ))];
        for k in ["shark", "shark", "grinder", "spender", "nibbler"]
            .iter()
            .take(players - 1)
        {
            agents.push(Box::new(FieldBot::preset(k, seed + 9)));
        }
        play_match(&mut agents, &MatchConfig::default(), seed);
    }
    let (mut bids, mut plays) = log.lock().unwrap().clone();
    bids.sort_by(|a, b| a.partial_cmp(b).unwrap());
    plays.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{players}p bid ms: p50 {:.1} p90 {:.1} p99 {:.1} max {:.1}",
        q(&bids, 0.5),
        q(&bids, 0.9),
        q(&bids, 0.99),
        bids[bids.len() - 1]
    );
    println!(
        "{players}p play ms: p50 {:.1} p90 {:.1} max {:.1}",
        q(&plays, 0.5),
        q(&plays, 0.9),
        plays[plays.len() - 1]
    );
}
