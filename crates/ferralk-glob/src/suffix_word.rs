//! Safe packed suffix comparisons for Apple Silicon.
//!
//! Common suffixes such as `.ts` are right-aligned once at pattern compile
//! time. Matching compares two masked `u64` words when the candidate is at
//! least 16 bytes long and keeps the ordinary scalar fallback for short paths.
//! On an Apple M1 Ultra with Rust 1.96.1 this matched the throughput of the
//! hand-written NEON prototype without lowering the crate's safety policy.

const WORD_BYTES: usize = 16;

/// A short suffix packed into the final bytes of a 16-byte block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedSuffix16 {
    expected: [u64; 2],
    required: [u64; 2],
    len: u8,
}

impl PreparedSuffix16 {
    pub(crate) fn new(suffix: &[u8]) -> Option<Self> {
        let len = u8::try_from(suffix.len()).ok()?;
        if suffix.is_empty() || suffix.len() > WORD_BYTES {
            return None;
        }
        let mut expected = [0; WORD_BYTES];
        let start = WORD_BYTES - suffix.len();
        expected[start..].copy_from_slice(suffix);
        let mut required = [0; WORD_BYTES];
        required[start..].fill(u8::MAX);
        Some(Self {
            expected: [
                u64::from_ne_bytes(expected[..8].try_into().expect("eight-byte half")),
                u64::from_ne_bytes(expected[8..].try_into().expect("eight-byte half")),
            ],
            required: [
                u64::from_ne_bytes(required[..8].try_into().expect("eight-byte half")),
                u64::from_ne_bytes(required[8..].try_into().expect("eight-byte half")),
            ],
            len,
        })
    }

    pub(crate) fn bytes(&self) -> [u8; WORD_BYTES] {
        let mut bytes = [0; WORD_BYTES];
        bytes[..8].copy_from_slice(&self.expected[0].to_ne_bytes());
        bytes[8..].copy_from_slice(&self.expected[1].to_ne_bytes());
        bytes
    }

    pub(crate) fn len(&self) -> usize {
        self.len.into()
    }

    /// Returns `None` when `path` is too short for a complete word load.
    pub(crate) fn matches(&self, path: &[u8]) -> Option<bool> {
        let candidate = path.get(path.len().checked_sub(WORD_BYTES)?..)?;
        let low = u64::from_ne_bytes(candidate[..8].try_into().expect("eight-byte half"));
        let high = u64::from_ne_bytes(candidate[8..].try_into().expect("eight-byte half"));
        Some(
            (((low ^ self.expected[0]) & self.required[0])
                | ((high ^ self.expected[1]) & self.required[1]))
                == 0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{PreparedSuffix16, WORD_BYTES};

    #[test]
    fn packed_suffix_matches_slice_semantics() {
        for suffix_len in 1..=WORD_BYTES {
            let suffix = (0..suffix_len)
                .map(|index| b'a' + index as u8)
                .collect::<Vec<_>>();
            let prepared = PreparedSuffix16::new(&suffix).expect("short suffix is supported");
            assert_eq!(
                &prepared.bytes()[WORD_BYTES - suffix_len..],
                suffix.as_slice()
            );

            for prefix_len in 0..=WORD_BYTES * 2 {
                let mut path = vec![b'x'; prefix_len];
                path.extend_from_slice(&suffix);
                assert_eq!(
                    prepared.matches(&path),
                    (path.len() >= WORD_BYTES).then(|| path.ends_with(&suffix)),
                    "suffix length {suffix_len}, prefix length {prefix_len}"
                );

                if path.len() >= WORD_BYTES {
                    for suffix_index in 0..suffix_len {
                        let path_index = prefix_len + suffix_index;
                        path[path_index] ^= 1;
                        assert_eq!(prepared.matches(&path), Some(false));
                        path[path_index] ^= 1;
                    }
                }
            }
        }
    }

    #[test]
    fn only_nonempty_short_suffixes_are_prepared() {
        assert!(PreparedSuffix16::new(b"").is_none());
        assert!(PreparedSuffix16::new(&[b'x'; WORD_BYTES + 1]).is_none());
    }
}
