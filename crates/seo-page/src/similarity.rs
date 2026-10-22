//! A MinHash signature of a page's main content, for near-duplicate
//! detection as Screaming Frog does it: the visible words outside header,
//! nav, footer and aside landmarks, in overlapping five-word shingles. Two
//! pages agree at a position of their signatures with the probability that
//! a shingle is common to both (their Jaccard similarity), so the share of
//! equal positions estimates it.
//!
//! One hash per shingle (one-permutation hashing, Li, Owen and Zhang 2012):
//! its top bits pick one of 128 bins, which keeps the smallest value it
//! gets; an empty bin borrows from the next full one (rotation, Shrivastava
//! and Li 2014). A signature keeps 16 bits a bin (b-bit minwise hashing):
//! two different minimums agree by chance once in 65536.

/// Values in a signature.
pub const SIGNATURE: usize = 128;
/// Words in a shingle.
const SHINGLE: usize = 5;

/// Builds a signature from words fed in document order.
pub(crate) struct MinHasher {
    /// The last `SHINGLE` word hashes, oldest at `words % SHINGLE`.
    window: [u64; SHINGLE],
    words: usize,
    shingles: usize,
    bins: [u64; SIGNATURE],
}

impl MinHasher {
    pub(crate) fn new() -> Self {
        MinHasher { window: [0; SHINGLE], words: 0, shingles: 0, bins: [u64::MAX; SIGNATURE] }
    }

    pub(crate) fn word(&mut self, word: &str) {
        self.window[self.words % SHINGLE] = xxhash_rust::xxh3::xxh3_64(word.as_bytes());
        self.words += 1;
        if self.words >= SHINGLE {
            self.shingle(SHINGLE);
        }
    }

    /// Hashes the last `length` words as one shingle into its bin.
    fn shingle(&mut self, length: usize) {
        let mut hash = 0u64;
        for at in 0..length {
            let word = self.window[(self.words + SHINGLE - length + at) % SHINGLE];
            hash = (hash ^ word).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(29);
        }
        // splitmix64's finaliser spreads the bits for the bin and the value.
        hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        hash ^= hash >> 31;
        let bin = (hash >> (64 - SIGNATURE.trailing_zeros())) as usize;
        let value = hash & ((1 << (64 - SIGNATURE.trailing_zeros())) - 1);
        if value < self.bins[bin] {
            self.bins[bin] = value;
        }
        self.shingles += 1;
    }

    /// The signature; empty without any word. Fewer words than a shingle
    /// make one shingle.
    pub(crate) fn finish(mut self) -> Vec<u16> {
        if self.shingles == 0 && self.words > 0 {
            self.shingle(self.words);
        }
        if self.shingles == 0 {
            return Vec::new();
        }
        (0..SIGNATURE)
            .map(|bin| {
                // An empty bin takes the next full one's value, changed by
                // the distance so two pages agree there only when they
                // agree on both the bin and the gap.
                let (distance, value) = (0..SIGNATURE)
                    .map(|step| (step as u64, self.bins[(bin + step) % SIGNATURE]))
                    .find(|(_, value)| *value != u64::MAX)
                    .expect("a shingle filled a bin");
                (value.wrapping_add(distance.wrapping_mul(0x9e37_79b9_7f4a_7c15)) >> 16) as u16
            })
            .collect()
    }
}

/// The estimated Jaccard similarity of two pages' shingles: the share of
/// positions where their signatures agree. 0 when either is missing.
pub fn similarity(a: &[u16], b: &[u16]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).filter(|(x, y)| x == y).count() as f64 / a.len() as f64
}

/// A signature as hex, four digits a value: compact in JSON and smaller
/// after compression than a list of numbers.
pub(crate) mod hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(values: &[u16], serializer: S) -> Result<S::Ok, S::Error> {
        let mut text = String::with_capacity(values.len() * 4);
        for value in values {
            text.push_str(&format!("{value:04x}"));
        }
        serializer.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u16>, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() % 4 != 0 || !text.is_ascii() {
            return Err(serde::de::Error::custom("a signature is four hex digits a value"));
        }
        (0..text.len())
            .step_by(4)
            .map(|at| u16::from_str_radix(&text[at..at + 4], 16).map_err(serde::de::Error::custom))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(text: &str) -> Vec<u16> {
        let mut hasher = MinHasher::new();
        for word in text.split_whitespace() {
            hasher.word(word);
        }
        hasher.finish()
    }

    #[test]
    fn similarity_tracks_shared_shingles() {
        let base: Vec<String> = (0..400).map(|at| format!("слово{at}")).collect();
        let text = base.join(" ");
        assert_eq!(similarity(&signature(&text), &signature(&text)), 1.0);
        // One word in forty changed: about 88% of the five-word shingles
        // survive in both, a Jaccard similarity near 0.78.
        let edited: Vec<String> =
            base.iter().enumerate().map(|(at, word)| if at % 40 == 0 { "другое".into() } else { word.clone() }).collect();
        let close = similarity(&signature(&text), &signature(&edited.join(" ")));
        assert!((0.65..0.9).contains(&close), "{close}");
        let other: Vec<String> = (0..400).map(|at| format!("иное{at}")).collect();
        assert!(similarity(&signature(&text), &signature(&other.join(" "))) < 0.05);
    }

    #[test]
    fn short_texts_and_empty_ones() {
        assert_eq!(signature("").len(), 0);
        assert_eq!(signature("два слова").len(), SIGNATURE);
        assert_eq!(similarity(&signature("два слова"), &signature("два слова")), 1.0);
        assert_eq!(similarity(&signature("два слова"), &[]), 0.0);
    }
}
