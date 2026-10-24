//! Print the value table and pacing at the start of a match.
use vickey_brain::{AuctionKind, Brain, FieldModel, Params, Tracker};
use vickey_core::{char_of, index, Rack};

fn main() {
    for n in [2usize, 6] {
        let ids: Vec<u64> = (1..=n as u64).collect();
        let mut t = Tracker::new(1, 1, AuctionKind::Vickrey, &ids, 5);
        t.set_my_rack(Rack::from_word("DIKLT"));
        t.on_auction_open(index('E').unwrap(), (98 - 5 * n as u32) - 1);
        let mut b = Brain::new(Params::default(), FieldModel::default(), 1);
        let d = b.bid(&t, index('E').unwrap());
        println!("n={n} {d:?}");
        let tab = b.value_table();
        let line: Vec<String> = (0..26)
            .map(|l| format!("{}:{:.1}", char_of(l), tab[l]))
            .collect();
        println!("  table {}", line.join(" "));
        let pr = vickey_brain::brain::letter_prior();
        println!(
            "  prior {}",
            (0..26)
                .map(|l| format!("{}:{:.1}", char_of(l), pr[l]))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}
