//! Generated names (M7): new sessions and the machines of VM tabs get an
//! adjective and a noun ("drifting cedar") instead of a number. They are
//! display names only: ids stay as they are, and renaming still works.
//!
//! 64 adjectives by 64 nouns is 4096 names; past that (or after many
//! collisions) a number is added, so a name is always found.

#[rustfmt::skip]
const ADJECTIVES: [&str; 64] = [
    "amber", "ancient", "autumn", "bitter", "bold", "brave", "bright", "calm", "clever", "cold", "crimson", "curious",
    "dancing", "dappled", "distant", "drifting", "dusty", "eager", "early", "electric", "fading", "falling", "fierce",
    "floating", "gentle", "gilded", "golden", "hidden", "hollow", "humble", "icy", "jolly", "lazy", "lively", "lone",
    "lucky", "misty", "mossy", "nimble", "noble", "patient", "polished", "proud", "quiet", "rapid", "restless",
    "rustling", "scarlet", "shy", "silent", "silver", "sleepy", "smooth", "snowy", "spinning", "steady", "still",
    "sunny", "swift", "tidy", "velvet", "wandering", "wild", "winding",
];

#[rustfmt::skip]
const NOUNS: [&str; 64] = [
    "acorn", "anchor", "aspen", "badger", "beacon", "birch", "bramble", "brook", "canyon", "cedar", "cinder", "cloud",
    "comet", "coral", "cove", "crane", "creek", "delta", "dune", "ember", "falcon", "fern", "finch", "fjord", "fox",
    "glacier", "grove", "harbor", "hazel", "heron", "island", "juniper", "kestrel", "lagoon", "lantern", "maple",
    "meadow", "mesa", "moth", "nebula", "otter", "owl", "pebble", "pine", "plover", "prairie", "quartz", "raven",
    "reef", "ridge", "river", "sparrow", "spruce", "summit", "thicket", "thistle", "tide", "tundra", "valley",
    "willow", "wren", "yarrow", "zephyr", "lichen",
];

/// A name not `taken`, picked from `seed` (any random number; the same seed
/// and taken set give the same name).
pub fn generate(seed: u64, taken: impl Fn(&str) -> bool) -> String {
    let mut x = seed;
    // A few hundred tries covers a nearly full set of names.
    for _ in 0..512 {
        x = mix(x);
        let name = format!("{} {}", ADJECTIVES[(x % 64) as usize], NOUNS[((x >> 6) % 64) as usize]);
        if !taken(&name) {
            return name;
        }
    }
    let base = format!("{} {}", ADJECTIVES[(x % 64) as usize], NOUNS[((x >> 6) % 64) as usize]);
    (2..).map(|n| format!("{base} {n}")).find(|n| !taken(n)).expect("some number is free")
}

/// splitmix64: spreads consecutive seeds across the whole range.
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn names_are_two_words_and_avoid_taken_ones() {
        let n = generate(1, |_| false);
        assert_eq!(n.split(' ').count(), 2, "{n}");
        let again = generate(1, |x| x == n);
        assert_ne!(again, n);
    }

    #[test]
    fn every_name_is_unique_even_past_the_word_lists() {
        let mut taken = HashSet::new();
        for i in 0..5000 {
            let n = generate(i, |x| taken.contains(x));
            assert!(taken.insert(n));
        }
        // No word is repeated within a list (that would shrink the space).
        assert_eq!(ADJECTIVES.iter().collect::<HashSet<_>>().len(), 64);
        assert_eq!(NOUNS.iter().collect::<HashSet<_>>().len(), 64);
    }
}
