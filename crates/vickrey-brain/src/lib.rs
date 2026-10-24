//! # vickrey-brain
//!
//! Strategy for the ScrabbleBot auction game: a per-match [`Tracker`] fed
//! by public events, a [`Market`] model of what rivals bid, and the
//! [`Brain`] that values letters by Monte Carlo over the best final word
//! partition, paces its budget, and decides when to cash words in.

// Fixed 26-slot letter arrays read most clearly as indexed loops.
#![allow(clippy::needless_range_loop)]

pub mod brain;
pub mod market;
pub mod params;
pub mod tracker;

pub use brain::{BidDecision, Brain};
pub use market::{BidDist, FieldModel, Market};
pub use params::Params;
pub use tracker::{AuctionKind, Player, Tracker};
