#![forbid(unsafe_code)]
//! Shared corpus model and the wire codec defined in `docs/corpus-format.md`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A single JSONL corpus record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// Stable, topic-local identifier.
    pub id: String,
    /// Glob or ignore expression, encoded with [`decode_bytes`].
    pub pattern: String,
    /// Candidate path, encoded with [`decode_bytes`].
    pub path: String,
    /// Behaviour switches in the compatibility matrix namespace.
    #[serde(default)]
    pub flags: Vec<String>,
    /// Whether the expression accepts the candidate path.
    pub expected: bool,
    /// Result produced by the named external oracle when it intentionally
    /// differs from ferralk's documented policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_expected: Option<bool>,
    /// Evidence used to establish the expected result.
    pub source: Source,
    /// Marks a recorded behaviour that has not been adopted as ferralk policy.
    #[serde(default)]
    pub disputed: bool,
    /// Human-readable context, especially for disagreements between oracles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Provenance of a corpus result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[serde(rename = "zlob_1_6_3")]
    Zlob163,
    FastGlob,
    GitCheckIgnore,
    Handwritten,
}

/// A strict decoding error for the `\\xNN` byte notation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteCodecError {
    offset: usize,
    message: &'static str,
}

impl ByteCodecError {
    /// Byte position in the UTF-8 input where decoding failed.
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// A short, stable explanation suitable for CLI output.
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for ByteCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for ByteCodecError {}

/// Decodes UTF-8 text plus canonical `\\xNN` byte escapes into raw bytes.
pub fn decode_bytes(input: &str) -> Result<Vec<u8>, ByteCodecError> {
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'\\' {
            let character = input[index..]
                .chars()
                .next()
                .expect("index is within a UTF-8 string");
            let mut buffer = [0; 4];
            result.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            index += character.len_utf8();
            continue;
        }

        if index + 3 >= bytes.len() || bytes[index + 1] != b'x' {
            return Err(ByteCodecError {
                offset: index,
                message: "expected \\xNN byte escape",
            });
        }

        let high = hex_value(bytes[index + 2]).ok_or(ByteCodecError {
            offset: index + 2,
            message: "expected hexadecimal digit",
        })?;
        let low = hex_value(bytes[index + 3]).ok_or(ByteCodecError {
            offset: index + 3,
            message: "expected hexadecimal digit",
        })?;
        result.push((high << 4) | low);
        index += 4;
    }

    Ok(result)
}

/// Encodes raw bytes as UTF-8 where possible and `\\xNN` otherwise.
pub fn encode_bytes(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len());
    let mut remaining = bytes;

    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                for character in valid.chars() {
                    push_character(&mut result, character);
                }
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                let (valid, invalid) = remaining.split_at(valid_up_to);
                for character in std::str::from_utf8(valid)
                    .expect("prefix is valid UTF-8")
                    .chars()
                {
                    push_character(&mut result, character);
                }
                if let Some(length) = error.error_len() {
                    for &byte in &invalid[..length] {
                        push_escape(&mut result, byte);
                    }
                    remaining = &invalid[length..];
                } else {
                    for &byte in invalid {
                        push_escape(&mut result, byte);
                    }
                    break;
                }
            }
        }
    }

    result
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn push_character(result: &mut String, character: char) {
    if character.is_ascii() && !character.is_ascii_control() && character != '\\' {
        result.push(character);
    } else {
        let mut buffer = [0; 4];
        for byte in character.encode_utf8(&mut buffer).bytes() {
            push_escape(result, byte);
        }
    }
}

fn push_escape(result: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    result.push('\\');
    result.push('x');
    result.push(char::from(HEX[usize::from(byte >> 4)]));
    result.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

#[cfg(test)]
mod tests {
    use super::{decode_bytes, encode_bytes};

    #[test]
    fn byte_codec_round_trips_mixed_utf8_and_non_utf8() {
        let bytes = b"alpha\\beta\x00\xff\xe2\x98\x83";
        let encoded = encode_bytes(bytes);
        assert_eq!(encoded, "alpha\\x5Cbeta\\x00\\xFF\\xE2\\x98\\x83");
        assert_eq!(decode_bytes(&encoded).unwrap(), bytes);
    }

    #[test]
    fn byte_codec_rejects_non_canonical_backslash_sequences() {
        assert!(decode_bytes("one\\two").is_err());
        assert!(decode_bytes("\\x0G").is_err());
        assert!(decode_bytes("\\x0").is_err());
    }
}
