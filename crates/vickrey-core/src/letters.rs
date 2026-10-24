//! Letter values, the bag, and the reward formula, mirrored from the
//! server's `letters.ts` so our arithmetic matches the module's exactly.

/// Index of a letter `A..=Z` in 0..26.
pub type Letter = usize;

pub const ALPHABET: usize = 26;

/// Standard Scrabble face values, A..Z.
pub const VALUE: [u32; ALPHABET] = [
    1, 3, 3, 2, 1, 4, 2, 4, 1, 8, 5, 1, 3, 1, 1, 3, 10, 1, 1, 1, 1, 4, 4, 8, 4, 10,
];

/// The 98-tile bag the server uses (standard distribution, no blanks).
pub const BAG: [u8; ALPHABET] = [
    9, 2, 2, 4, 12, 2, 3, 2, 9, 1, 1, 4, 2, 6, 8, 2, 1, 6, 4, 6, 4, 2, 2, 1, 2, 1,
];

pub const BAG_SIZE: u32 = 98;
pub const STARTING_BALANCE: i64 = 100;
pub const STARTING_RACK: usize = 5;
pub const VOWELS: [Letter; 5] = [0, 4, 8, 14, 20];

/// `'A'..='Z'` (either case) to an index.
#[inline]
pub fn index(c: char) -> Option<Letter> {
    let u = c.to_ascii_uppercase();
    u.is_ascii_uppercase().then(|| (u as u8 - b'A') as Letter)
}

#[inline]
pub fn char_of(l: Letter) -> char {
    (b'A' + l as u8) as char
}

/// Length multiplier as `(numerator, denominator)`: 1x up to three
/// letters, then 1.5x, 2x, 2.5x and 3x from seven letters on.
#[inline]
pub fn multiplier(len: usize) -> (u32, u32) {
    match len {
        0..=3 => (1, 1),
        4 => (3, 2),
        5 => (2, 1),
        6 => (5, 2),
        _ => (3, 1),
    }
}

/// The reward the server pays for a word with this face-value sum and
/// length (integer division, like the module).
#[inline]
pub fn reward(base: u32, len: usize) -> u32 {
    let (n, d) = multiplier(len);
    base * n / d
}

/// Reward for a concrete word. Assumes `A..=Z` only.
pub fn word_reward(word: &str) -> u32 {
    let base: u32 = word.chars().filter_map(index).map(|l| VALUE[l]).sum();
    reward(base, word.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bag_has_98_tiles() {
        assert_eq!(BAG.iter().map(|&n| n as u32).sum::<u32>(), BAG_SIZE);
    }

    #[test]
    fn rewards_match_server_examples() {
        // From live word_play rows: ACHY base 12 -> 18, CAZIQUE -> 81.
        assert_eq!(word_reward("ACHY"), 18);
        assert_eq!(word_reward("CAZIQUE"), 81);
        assert_eq!(word_reward("AX"), 9);
        assert_eq!(word_reward("QUIZ"), 33); // 22 * 1.5
        assert_eq!(word_reward("JAB"), 12);
        assert_eq!(reward(7, 4), 10); // 10.5 truncates
    }
}
