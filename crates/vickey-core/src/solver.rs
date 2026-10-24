//! Exact "best set of words" for a rack.
//!
//! The final dump of a match plays every word we can, so a rack's terminal
//! worth is the maximum total reward over sets of disjoint dictionary words
//! drawn from it. We compute it with a memoised recursion that branches on
//! the rack's most valuable remaining letter `x`:
//!
//! ```text
//! f(R) = max( f(R - x),                         // x goes unplayed
//!             max_{w ∋ x, w ⊆ R} reward(w) + f(R - w) )
//! ```
//!
//! Each partition is reached once, branches are visited in descending
//! optimistic order, and the optimistic bound `3 · face(R)` prunes the rest,
//! so racks of 20+ letters solve in well under a millisecond.

use crate::dict::Dictionary;
use crate::letters::{multiplier, Letter, ALPHABET, VALUE};
use crate::rack::Rack;

/// A chosen set of words and their total reward.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Partition {
    pub score: u32,
    /// Signature ids into `Dictionary::sigs`.
    pub words: Vec<u32>,
}

impl Partition {
    pub fn word_strings(&self, d: &Dictionary) -> Vec<&'static str> {
        self.words.iter().map(|&i| d.sig(i).word).collect()
    }
}

/// Union of the letters used by `live` candidates.
#[inline]
fn usable_mask(d: &Dictionary, live: &[u32]) -> u32 {
    live.iter().fold(0, |m, &i| m | d.sig(i).mask)
}

/// Optimistic bound: every usable letter scores at the best multiplier the
/// usable letter count allows.
#[inline]
fn upper_bound(r: &Rack, usable: u32) -> u32 {
    let mut n = 0usize;
    let mut face = 0u32;
    for l in 0..ALPHABET {
        if usable & (1 << l) != 0 {
            n += r.0[l] as usize;
            face += r.0[l] as u32 * VALUE[l];
        }
    }
    if n < 2 {
        return 0;
    }
    let (num, den) = multiplier(n);
    face * num / den
}

/// Highest-value letter among `usable`; ties go to the scarcer one.
#[inline]
fn pivot(r: &Rack, usable: u32) -> Option<Letter> {
    let mut best: Option<Letter> = None;
    for l in 0..ALPHABET {
        if r.0[l] == 0 || usable & (1 << l) == 0 {
            continue;
        }
        best = match best {
            None => Some(l),
            Some(b) if VALUE[l] > VALUE[b] || (VALUE[l] == VALUE[b] && r.0[l] < r.0[b]) => Some(l),
            keep => keep,
        };
    }
    best
}

/// Anytime branch-and-bound solver. `solve` is exact when it finishes
/// within `node_budget`; otherwise it returns the best partition found,
/// which starts from the greedy answer and only improves.
pub struct Solver<'d> {
    dict: &'d Dictionary,
    /// Minimum word length to consider (2 = everything the server accepts).
    pub min_len: usize,
    pub node_budget: u64,
    /// Optional wall-clock limit, checked every 1024 nodes.
    pub deadline: Option<std::time::Instant>,
    pub nodes: u64,
    best: u32,
    best_words: Vec<u32>,
    path: Vec<u32>,
}

impl<'d> Solver<'d> {
    pub fn new(dict: &'d Dictionary) -> Self {
        Solver {
            dict,
            min_len: 2,
            node_budget: 2_000_000,
            deadline: None,
            nodes: 0,
            best: 0,
            best_words: Vec::new(),
            path: Vec::new(),
        }
    }

    pub fn with_budget(dict: &'d Dictionary, node_budget: u64) -> Self {
        Solver {
            node_budget,
            ..Solver::new(dict)
        }
    }

    /// A solver that stops after `ms` milliseconds of wall-clock time.
    pub fn with_deadline(dict: &'d Dictionary, ms: u64) -> Self {
        Solver {
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(ms)),
            ..Solver::new(dict)
        }
    }

    /// Whether the last `solve` proved optimality.
    pub fn exhaustive(&self) -> bool {
        self.nodes < self.node_budget
    }

    /// The best partition of `rack`.
    pub fn solve(&mut self, rack: &Rack) -> Partition {
        let cands = self.dict.candidates(rack, self.min_len);
        let g = greedy_from(self.dict, rack, &cands);
        self.best = g.score;
        self.best_words = g.words;
        self.nodes = 0;
        self.path.clear();
        let budget = self.node_budget;
        let mut sorted = cands;
        sorted.sort_unstable_by(|&a, &b| self.dict.sig(b).reward.cmp(&self.dict.sig(a).reward));
        self.dfs(*rack, &sorted, 0);
        let done = self.nodes < self.node_budget;
        self.node_budget = budget;
        if !done {
            self.nodes = budget;
        }
        Partition {
            score: self.best,
            words: self.best_words.clone(),
        }
    }

    pub fn value(&mut self, rack: &Rack) -> u32 {
        self.solve(rack).score
    }

    fn dfs(&mut self, r: Rack, cands: &[u32], acc: u32) {
        self.nodes += 1;
        if self.nodes >= self.node_budget {
            return;
        }
        if self.nodes & 1023 == 0 {
            if let Some(dl) = self.deadline {
                if std::time::Instant::now() >= dl {
                    self.node_budget = self.nodes;
                    return;
                }
            }
        }
        let live: Vec<u32> = cands
            .iter()
            .copied()
            .filter(|&i| r.contains(&self.dict.sig(i).rack))
            .collect();
        if acc > self.best {
            self.best = acc;
            self.best_words = self.path.clone();
        }
        let usable = usable_mask(self.dict, &live);
        if acc + upper_bound(&r, usable) <= self.best {
            return;
        }
        let Some(x) = pivot(&r, usable) else { return };
        // `live` is sorted by reward, so branches come best-first.
        for k in 0..live.len() {
            let i = live[k];
            let s = self.dict.sig(i);
            if s.rack.0[x] == 0 {
                continue;
            }
            let mut rest = r;
            rest.sub(&s.rack);
            if acc + s.reward as u32 + upper_bound(&rest, usable) <= self.best {
                continue;
            }
            self.path.push(i);
            self.dfs(rest, &live, acc + s.reward as u32);
            self.path.pop();
            if self.nodes >= self.node_budget {
                return;
            }
        }
        // x goes unplayed.
        let mut dropped = r;
        dropped.0[x] -= 1;
        self.dfs(dropped, &live, acc);
    }
}

/// Greedy: repeatedly play the highest-reward word that still fits.
pub fn greedy(dict: &Dictionary, rack: &Rack) -> Partition {
    let cands = dict.candidates(rack, 2);
    greedy_from(dict, rack, &cands)
}

fn greedy_from(dict: &Dictionary, rack: &Rack, cands: &[u32]) -> Partition {
    let mut r = *rack;
    let mut live: Vec<u32> = cands.to_vec();
    let mut p = Partition::default();
    loop {
        live.retain(|&i| r.contains(&dict.sig(i).rack));
        let Some(&best) = live
            .iter()
            .max_by_key(|&&i| (dict.sig(i).reward, std::cmp::Reverse(dict.sig(i).len)))
        else {
            break;
        };
        let s = dict.sig(best);
        r.sub(&s.rack);
        p.score += s.reward as u32;
        p.words.push(best);
    }
    p
}

/// One-shot exact (budgeted) solve.
pub fn best_partition(dict: &Dictionary, rack: &Rack) -> Partition {
    Solver::new(dict).solve(rack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::dictionary;
    use crate::letters::BAG;
    use rand::rngs::SmallRng;
    use rand::{Rng, SeedableRng};

    /// Plain exhaustive search, for cross-checking on small racks.
    fn brute(d: &Dictionary, r: Rack) -> u32 {
        let cands = d.candidates(&r, 2);
        fn go(d: &Dictionary, r: Rack, cands: &[u32], from: usize) -> u32 {
            let mut best = 0;
            for (j, &i) in cands.iter().enumerate().skip(from) {
                let s = d.sig(i);
                if r.contains(&s.rack) {
                    let mut rest = r;
                    rest.sub(&s.rack);
                    best = best.max(s.reward as u32 + go(d, rest, cands, j));
                }
            }
            best
        }
        go(d, r, &cands, 0)
    }

    fn random_rack(rng: &mut SmallRng, n: usize) -> Rack {
        let mut pool: Vec<usize> = (0..26)
            .flat_map(|l| std::iter::repeat(l).take(BAG[l] as usize))
            .collect();
        let mut r = Rack::EMPTY;
        for _ in 0..n {
            let j = rng.gen_range(0..pool.len());
            r.add(pool.swap_remove(j));
        }
        r
    }

    #[test]
    fn matches_brute_force_on_small_racks() {
        let d = dictionary();
        let mut rng = SmallRng::seed_from_u64(7);
        for n in 2..=9 {
            for _ in 0..25 {
                let r = random_rack(&mut rng, n);
                let p = best_partition(d, &r);
                assert_eq!(p.score, brute(d, r), "rack {r:?}");
            }
        }
    }

    #[test]
    fn partition_words_are_disjoint_and_valid() {
        let d = dictionary();
        let mut rng = SmallRng::seed_from_u64(11);
        for _ in 0..50 {
            let r = random_rack(&mut rng, 22);
            let p = best_partition(d, &r);
            let mut used = Rack::EMPTY;
            let mut total = 0;
            for w in p.word_strings(d) {
                assert!(d.is_valid(w));
                used.plus(&Rack::from_word(w));
                total += crate::letters::word_reward(w);
            }
            assert!(r.contains(&used));
            assert_eq!(total, p.score);
        }
    }

    #[test]
    fn known_rack() {
        let d = dictionary();
        let p = best_partition(d, &Rack::from_word("CAZIQUEAT"));
        assert!(p.score >= 81 + 2);
    }
}
