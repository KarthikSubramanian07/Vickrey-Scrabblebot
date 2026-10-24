//! # vickey-core
//!
//! The pure rules of ScrabbleBot: letter values and the reward formula,
//! racks, the ENABLE dictionary indexed by letter multiset, and an exact
//! solver for the best set of words a rack can play.

pub mod dict;
pub mod letters;
pub mod rack;
pub mod solver;

pub use dict::{dictionary, Dictionary, Sig};
pub use letters::{char_of, index, reward, word_reward, Letter, ALPHABET, BAG, VALUE};
pub use rack::Rack;
pub use solver::{best_partition, Partition, Solver};
