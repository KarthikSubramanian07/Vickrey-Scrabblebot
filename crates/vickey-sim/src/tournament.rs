//! The tournament format from `tournament.ts`: Swiss rounds at tables of
//! about `match_size` (points = match_size − place, floored at 0), a cut to
//! the top `top_cut`, elimination matches that knock out last place until
//! two remain, then a three-game final decided by total score.

use crate::arena::make_agent;
use crate::engine::{play_match, MatchConfig};
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use vickey_brain::{FieldModel, Params};

#[derive(Clone, Debug)]
pub struct TournamentConfig {
    pub match_size: usize,
    pub swiss_rounds: usize,
    pub top_cut: usize,
    pub final_games: usize,
    pub cfg: MatchConfig,
}

impl Default for TournamentConfig {
    fn default() -> Self {
        TournamentConfig {
            match_size: 6,
            swiss_rounds: 4,
            top_cut: 8,
            final_games: 3,
            cfg: MatchConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    spec: String,
    swiss: i64,
    total: i64,
    alive: bool,
}

/// `tournament.ts: tableSizes`.
pub fn table_sizes(n: usize, match_size: usize) -> Vec<usize> {
    if n < 2 || match_size == 0 {
        return Vec::new();
    }
    let tables = (n / match_size).max(1);
    let base = n / tables;
    let extra = n % tables;
    (0..tables).map(|i| base + usize::from(i < extra)).collect()
}

/// Runs one tournament and returns entrant specs in finishing order.
pub fn run_tournament(
    specs: &[String],
    params_by_label: &(dyn Fn(&str) -> Params + Sync),
    field: &FieldModel,
    tc: &TournamentConfig,
    seed: u64,
) -> Vec<String> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut entries: Vec<Entry> = specs
        .iter()
        .map(|s| Entry {
            spec: s.clone(),
            swiss: 0,
            total: 0,
            alive: true,
        })
        .collect();
    let mut match_seed = seed.wrapping_mul(7919);
    let mut play = |idx: &[usize], entries: &mut Vec<Entry>| -> Vec<i64> {
        match_seed = match_seed.wrapping_add(1);
        let mut agents: Vec<_> = idx
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let spec = &entries[i].spec;
                let label = spec.split_once('@').map(|x| x.1).unwrap_or(spec);
                make_agent(
                    spec,
                    &params_by_label(label),
                    field,
                    match_seed ^ (k as u64 + 1) * 31,
                )
            })
            .collect();
        let r = play_match(&mut agents, &tc.cfg, match_seed);
        r.seats.iter().map(|s| s.score).collect()
    };

    // Swiss.
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.shuffle(&mut rng);
    for round in 0..tc.swiss_rounds {
        if round > 0 {
            order.sort_by(|&a, &b| {
                entries[b]
                    .swiss
                    .cmp(&entries[a].swiss)
                    .then(entries[b].total.cmp(&entries[a].total))
                    .then(a.cmp(&b))
            });
        }
        let mut rest = order.clone();
        for size in table_sizes(rest.len(), tc.match_size) {
            let table: Vec<usize> = rest.drain(..size).collect();
            let scores = play(&table, &mut entries);
            for (k, &i) in table.iter().enumerate() {
                let place = scores.iter().filter(|&&s| s > scores[k]).count();
                entries[i].swiss += (tc.match_size as i64 - place as i64).max(0);
                entries[i].total += scores[k];
            }
        }
    }
    // Cut.
    let mut ranked: Vec<usize> = (0..entries.len()).collect();
    ranked.sort_by(|&a, &b| {
        entries[b]
            .swiss
            .cmp(&entries[a].swiss)
            .then(entries[b].total.cmp(&entries[a].total))
            .then(a.cmp(&b))
    });
    let mut finish: Vec<usize> = Vec::new();
    for &i in ranked.iter().skip(tc.top_cut) {
        entries[i].alive = false;
    }
    let mut alive: Vec<usize> = ranked.iter().copied().take(tc.top_cut).collect();
    // Elimination.
    while alive.len() > 2 {
        let scores = play(&alive, &mut entries);
        for (k, &i) in alive.iter().enumerate() {
            entries[i].total += scores[k];
        }
        let worst = (0..alive.len())
            .min_by(|&a, &b| {
                scores[a]
                    .cmp(&scores[b])
                    .then(entries[alive[a]].total.cmp(&entries[alive[b]].total))
                    .then(alive[b].cmp(&alive[a]))
            })
            .unwrap();
        finish.push(alive.remove(worst));
    }
    // Final.
    let mut fin = [0i64; 2];
    for _ in 0..tc.final_games {
        let scores = play(&alive, &mut entries);
        fin[0] += scores[0];
        fin[1] += scores[1];
    }
    let (champ, runner) = if fin[0] > fin[1]
        || (fin[0] == fin[1] && entries[alive[0]].total >= entries[alive[1]].total)
    {
        (alive[0], alive[1])
    } else {
        (alive[1], alive[0])
    };
    finish.push(runner);
    finish.push(champ);
    finish.reverse();
    let mut out: Vec<String> = finish.iter().map(|&i| entries[i].spec.clone()).collect();
    out.extend(
        ranked
            .iter()
            .skip(tc.top_cut)
            .map(|&i| entries[i].spec.clone()),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_sizes_match_server() {
        assert_eq!(table_sizes(7, 3), vec![4, 3]);
        assert_eq!(table_sizes(12, 6), vec![6, 6]);
        assert_eq!(table_sizes(13, 6), vec![7, 6]);
        assert_eq!(table_sizes(1, 6), Vec::<usize>::new());
    }
}
