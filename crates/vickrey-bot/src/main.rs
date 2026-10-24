//! `vickrey`: the live ScrabbleBot client.
//!
//! - `vickrey run`      autopilot daemon (lobby, tournaments, bidding, words, telemetry)
//! - `vickrey run --spectate <bot_id|all>`  shadow live matches without calling reducers
//! - `vickrey setup`    zero-browser onboarding: team, bot, credential, token file
//! - `vickrey status`   who we are, rating, lobby, tournaments
//! - `vickrey replay`   what our brain would have bid in a past match
//! - `vickrey ab-report` the live A/B scoreboard (champion vs challenger)

mod ab;
mod config;
mod conn;
mod engine;
mod helpers;
#[allow(warnings, clippy::all, clippy::pedantic, clippy::nursery)]
#[rustfmt::skip]
mod module_bindings;
mod replay;
mod setup;
mod status;
mod telemetry;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Common;
use engine::{Engine, Mode, RunOpts, Shadow};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "vickrey",
    version,
    about = "Vickrey: an auction-savvy ScrabbleBot for SpacetimeDB"
)]
struct Cli {
    #[command(flatten)]
    common: Common,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the autopilot: join lobbies and tournaments, bid, play words, log telemetry.
    Run {
        /// Don't join the casual lobby.
        #[arg(long)]
        no_lobby: bool,
        /// Don't auto-register for tournaments.
        #[arg(long)]
        no_tournaments: bool,
        /// Shadow a bot (or `all` running matches) anonymously: compute our
        /// decisions but never call a reducer. Telemetry goes to runs/spectate/.
        #[arg(long, value_name = "BOT_ID|all")]
        spectate: Option<Shadow>,
        /// Stop after this many seconds (default: run forever).
        #[arg(long)]
        duration: Option<u64>,
        /// Exit non-zero after this many consecutive failed reconnects.
        #[arg(long, default_value_t = 20)]
        max_reconnect_failures: u32,
        /// Warn when a decision takes longer than this (ms).
        #[arg(long, default_value_t = 400.0)]
        slow_ms: f64,
        /// Value with a quarter of the Monte Carlo samples when fewer than
        /// this many ms remain before the auction closes at decision start.
        #[arg(long, default_value_t = 550)]
        fast_below_ms: i64,
    },
    /// Create team + bot if needed, mint and claim a credential, save the bot token.
    Setup {
        #[arg(long)]
        team: String,
        #[arg(long)]
        bot: String,
        /// Overwrite an existing token file.
        #[arg(long)]
        force: bool,
    },
    /// Show our bot, rating, the open lobby and tournaments (works anonymously).
    Status,
    /// Live A/B scoreboard: champion (params.json) vs the challenger in ab.json.
    AbReport,
    /// Rebuild a match from public history and print the bids our brain would make.
    Replay {
        match_id: u64,
        /// Bot to impersonate (default: the match winner).
        #[arg(long = "as")]
        as_bot: Option<u64>,
    },
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    if let Err(e) = dispatch(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Run {
            no_lobby,
            no_tournaments,
            spectate,
            duration,
            max_reconnect_failures,
            slow_ms,
            fast_below_ms,
        } => {
            let mode = spectate.map(Mode::Spectate).unwrap_or(Mode::Live);
            let stats = Engine::new(RunOpts {
                common: cli.common,
                mode,
                no_lobby,
                no_tournaments,
                duration: duration.map(Duration::from_secs),
                max_reconnect_failures,
                slow_ms,
                fast_below_ms,
            })
            .run()?;
            println!("{}", stats.report());
            Ok(())
        }
        Cmd::Setup { team, bot, force } => setup::setup(&cli.common, &team, &bot, force),
        Cmd::Status => status::status(&cli.common),
        Cmd::AbReport => {
            ab::report(&cli.common.config_dir, &cli.common.runs_dir);
            Ok(())
        }
        Cmd::Replay { match_id, as_bot } => replay::replay(&cli.common, match_id, as_bot),
    }
}
