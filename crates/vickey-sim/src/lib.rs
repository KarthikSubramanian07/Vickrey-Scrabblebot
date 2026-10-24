//! # vickey-sim
//!
//! A server-faithful simulator for ScrabbleBot: the match loop in
//! [`engine`], opponents in [`agents`], tournament formats in
//! [`tournament`], and the evaluation/tuning harness in [`arena`].

// Fixed 26-slot letter arrays read most clearly as indexed loops.
#![allow(clippy::needless_range_loop)]

pub mod agents;
pub mod arena;
pub mod engine;
pub mod live;
pub mod tournament;
pub mod tune;

pub use agents::{Agent, FieldBot, ServerSim, SimStrategy, Starter, Vickey};
pub use engine::{play_match, MatchConfig, MatchResult};
