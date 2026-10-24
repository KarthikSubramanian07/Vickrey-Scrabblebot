//! `cargo run -p vickey-core --release --example bench_solver`
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::time::Instant;
use vickey_core::{dictionary, solver::greedy, Rack, Solver, BAG};

fn main() {
    let d = dictionary();
    let mut rng = SmallRng::seed_from_u64(1);
    let t = Instant::now();
    let _ = d.candidates(&Rack::from_word("ABCDEFGHIJKLMNOPQRSTUVWXYZ"), 2);
    println!("candidates(26 distinct): {:?}", t.elapsed());
    for n in [7usize, 10, 15, 20, 25, 30, 40] {
        let mut worst = 0u128;
        let mut total = 0u128;
        let mut score = 0u64;
        let mut gscore = 0u64;
        let mut gus = 0u128;
        let mut inexact = 0;
        let reps = 40;
        for _ in 0..reps {
            let mut pool: Vec<usize> = (0..26)
                .flat_map(|l| std::iter::repeat(l).take(BAG[l] as usize))
                .collect();
            let mut r = Rack::EMPTY;
            for _ in 0..n {
                let j = rng.gen_range(0..pool.len());
                r.add(pool.swap_remove(j));
            }
            let mut s = Solver::new(d);
            let t = Instant::now();
            score += s.solve(&r).score as u64;
            let us = t.elapsed().as_micros();
            if !s.exhaustive() {
                inexact += 1;
            }
            let tg = Instant::now();
            gscore += greedy(d, &r).score as u64;
            gus += tg.elapsed().as_micros();
            worst = worst.max(us);
            total += us;
        }
        println!("n={n:2}  mean {:>7}us  worst {:>8}us  avg score {:.1}  inexact {inexact}/{reps}  | greedy {:>5}us avg {:.1}", total / reps, worst, score as f64 / reps as f64, gus / reps, gscore as f64 / reps as f64);
    }
}
