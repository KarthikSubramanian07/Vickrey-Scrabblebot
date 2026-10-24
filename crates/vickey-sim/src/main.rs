//! `vickey-arena`: run simulated matches, tournaments and the tuner.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Instant;
use vickey_brain::{AuctionKind, FieldModel, Params};
use vickey_sim::arena::{print_stats, run_lineup};
use vickey_sim::engine::MatchConfig;

#[derive(Parser)]
#[command(
    name = "vickey-arena",
    about = "Simulate ScrabbleBot matches and tune Vickey"
)]
struct Cli {
    /// Strategy params for `vickey` seats.
    #[arg(long, global = true, default_value = "config/params.json")]
    params: PathBuf,
    /// Alternative params for seats labelled `vickey@b`.
    #[arg(long, global = true)]
    params_b: Option<PathBuf>,
    /// Field price model.
    #[arg(long, global = true, default_value = "config/field.json")]
    field: PathBuf,
    #[arg(long, global = true)]
    first_price: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Play `n` matches of a lineup, e.g. `vickey,shark,shark,grinder,spender,nibbler`.
    Bench {
        #[arg(long)]
        lineup: String,
        #[arg(short, long, default_value_t = 100)]
        n: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Play one match with a decision trace for the first `vickey` seat.
    Trace {
        #[arg(long)]
        lineup: String,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Hill-climb the params for one profile, saving every improvement.
    Tune {
        #[arg(long, default_value = "multi")]
        profile: String,
        #[arg(long, default_value_t = 30)]
        iters: u32,
        #[arg(long, default_value_t = 48)]
        matches: u32,
        /// Where the incumbent is written whenever it improves.
        #[arg(long)]
        out: PathBuf,
        /// JSONL log of every candidate.
        #[arg(long, default_value = "runs/tune-history.jsonl")]
        history: PathBuf,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Paired comparison of `--params` (cand) against `--params-b` (incumbent).
    Compare {
        #[arg(long, default_value = "multi")]
        profile: String,
        #[arg(short, long, default_value_t = 64)]
        n: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Print the compiled-in default params as JSON.
    Defaults,
    /// Run whole tournaments and report how often each entrant places.
    Tournament {
        #[arg(long)]
        entrants: String,
        #[arg(short, long, default_value_t = 20)]
        n: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
}

fn parse_profile(s: &str) -> vickey_sim::tune::Profile {
    if s == "duel" {
        vickey_sim::tune::Profile::Duel
    } else {
        vickey_sim::tune::Profile::Multi
    }
}

fn load_params(p: &PathBuf) -> Params {
    Params::load(p).unwrap_or_default()
}

fn load_field(p: &PathBuf) -> FieldModel {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let a = load_params(&cli.params);
    let b = cli
        .params_b
        .as_ref()
        .map(load_params)
        .unwrap_or_else(|| a.clone());
    let field = load_field(&cli.field);
    let cfg = MatchConfig {
        kind: if cli.first_price {
            AuctionKind::FirstPrice
        } else {
            AuctionKind::Vickrey
        },
        ..MatchConfig::default()
    };
    let by_label = move |label: &str| if label == "b" { b.clone() } else { a.clone() };
    match cli.cmd {
        Cmd::Bench { lineup, n, seed } => {
            let lineup: Vec<String> = lineup.split(',').map(|s| s.trim().to_string()).collect();
            let t = Instant::now();
            let stats = run_lineup(&lineup, &by_label, &field, &cfg, n, seed);
            print_stats(&stats);
            eprintln!("{n} matches in {:.1}s", t.elapsed().as_secs_f64());
        }
        Cmd::Trace { lineup, seed } => {
            let specs: Vec<String> = lineup.split(',').map(|s| s.trim().to_string()).collect();
            let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut agents: Vec<Box<dyn vickey_sim::Agent>> = Vec::new();
            let mut traced = false;
            for (i, spec) in specs.iter().enumerate() {
                if spec.starts_with("vickey") && !traced {
                    traced = true;
                    let label = spec.split_once('@').map(|x| x.1).unwrap_or(spec);
                    let mut v =
                        vickey_sim::Vickey::new(label, by_label(label), field.clone(), seed);
                    v.trace = Some(log.clone());
                    agents.push(Box::new(v));
                } else {
                    let label = spec.split_once('@').map(|x| x.1).unwrap_or(spec);
                    agents.push(vickey_sim::arena::make_agent(
                        spec,
                        &by_label(label),
                        &field,
                        seed ^ (i as u64 + 1) * 0x9e37,
                    ));
                }
            }
            let r = vickey_sim::play_match(&mut agents, &cfg, seed);
            for line in log.lock().unwrap().iter() {
                println!("{line}");
            }
            for (i, s) in r.seats.iter().enumerate() {
                println!(
                    "{:<10} score {:>4} place {} letters {:>2} spent {:>4} left {:>4} words {:?}",
                    s.name,
                    s.score,
                    r.place(i),
                    s.letters_won,
                    s.spent,
                    s.balance,
                    s.words
                );
            }
        }
        Cmd::Tune {
            profile,
            iters,
            matches,
            out,
            history,
            seed,
        } => {
            use std::io::Write;
            let prof = parse_profile(&profile);
            let start = Params::load(&out).unwrap_or_else(|_| load_params(&cli.params));
            if let Some(dir) = history.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&history)?;
            let out2 = out.clone();
            let mut on_step = |step: &vickey_sim::tune::Step, inc: &Params| {
                let line = serde_json::json!({"profile": profile, "step": step});
                let _ = writeln!(log, "{line}");
                eprintln!(
                    "iter {:>3} edge {:+.3} ± {:.3} (t {:+.2}) {}",
                    step.iter,
                    step.edge.mean,
                    step.edge.se,
                    step.edge.t(),
                    if step.accepted { "ACCEPTED" } else { "" }
                );
                if step.accepted {
                    let _ = inc.save(&out2);
                }
            };
            let best = vickey_sim::tune::tune(
                prof,
                start,
                &field,
                &cfg,
                iters,
                matches,
                seed,
                &mut on_step,
            );
            best.save(&out).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&best)?);
        }
        Cmd::Compare { profile, n, seed } => {
            let cand = load_params(&cli.params);
            let inc = cli.params_b.as_ref().map(load_params).unwrap_or_default();
            let e = vickey_sim::tune::evaluate(
                parse_profile(&profile),
                &cand,
                &inc,
                &field,
                &cfg,
                n,
                seed,
            );
            println!(
                "edge {:+.3} ± {:.3} (t {:+.2}, n {})",
                e.mean,
                e.se,
                e.t(),
                e.n
            );
        }
        Cmd::Defaults => println!("{}", serde_json::to_string_pretty(&Params::default())?),
        Cmd::Tournament { entrants, n, seed } => {
            let specs: Vec<String> = entrants.split(',').map(|s| s.trim().to_string()).collect();
            let tc = vickey_sim::tournament::TournamentConfig {
                cfg: cfg.clone(),
                ..Default::default()
            };
            let mut tally: std::collections::BTreeMap<String, [u32; 4]> = Default::default();
            for i in 0..n {
                let order = vickey_sim::tournament::run_tournament(
                    &specs,
                    &by_label,
                    &field,
                    &tc,
                    seed + i as u64,
                );
                for (pos, s) in order.iter().enumerate().take(3) {
                    tally.entry(s.clone()).or_default()[pos] += 1;
                }
                tally.entry(order[0].clone()).or_default()[3] += 0;
            }
            println!("{:<18} {:>5} {:>5} {:>5}", "entrant", "1st", "2nd", "3rd");
            for (s, t) in tally {
                println!("{:<18} {:>5} {:>5} {:>5}", s, t[0], t[1], t[2]);
            }
        }
    }
    Ok(())
}
