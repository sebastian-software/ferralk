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
    /// Operation described by this record. Existing records default to a full
    /// matcher verdict for backwards-compatible JSONL decoding.
    #[serde(default)]
    pub kind: CaseKind,
    /// Input candidates for a [`CaseKind::MatchPaths`] operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Ferralk's expected selected candidates for a list operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<String>,
    /// The external oracle's selected candidates when it intentionally differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_matches: Option<Vec<String>>,
    /// Base directory stripped from input candidates for a
    /// [`CaseKind::MatchPathsAt`] operation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_path: String,
    /// Ferralk-selected input positions for an index-list operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indices: Vec<usize>,
    /// The external oracle's selected indices when it intentionally differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_indices: Option<Vec<usize>>,
    /// Glob or ignore expression, encoded with [`decode_bytes`].
    pub pattern: String,
    /// Candidate path, encoded with [`decode_bytes`].
    pub path: String,
    /// Behaviour switches in the compatibility matrix namespace.
    #[serde(default)]
    pub flags: Vec<String>,
    /// Newline-delimited rules placed in the synthetic `.gitignore` by the
    /// Git oracle. Used only by `ignore.jsonl` cases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_rules: Vec<String>,
    /// Further `.gitignore` files below the root, in the order Git reads them.
    ///
    /// Git consults the ignore file closest to the candidate last, so a deeper
    /// file overrides a shallower one. Only these records can express that.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested_ignore_rules: Vec<NestedIgnoreFile>,
    /// Repository-wide excludes, written to `.git/info/exclude`.
    ///
    /// Git reads them before any `.gitignore`, so every ignore file overrides
    /// them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_rules: Vec<String>,
    /// Whether the expression accepts the candidate path. A
    /// [`CaseKind::CompileError`] case never accepts and records `false`.
    pub expected: bool,
    /// Expected byte offset of the rejected construct for a
    /// [`CaseKind::CompileError`] case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_offset: Option<usize>,
    /// Expected stable error text for a [`CaseKind::CompileError`] case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Restricts a case to the platform whose separator set it describes.
    ///
    /// Absent means the verdict is platform-independent and every host
    /// replays it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
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

/// The operation a corpus record verifies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// A complete pattern/path match.
    #[default]
    Matcher,
    /// Flag-sensitive preflight detection of active wildcard syntax.
    HasWildcards,
    /// Filters a caller-owned list of candidate paths.
    MatchPaths,
    /// Filters full candidate paths after stripping an explicit base directory.
    MatchPathsAt,
    /// Returns the positions of accepted input paths.
    MatchPathIndices,
    /// Returns accepted input positions after stripping an explicit base directory.
    MatchPathIndicesAt,
    /// A pattern the compiler must reject, optionally at a recorded offset
    /// and with a recorded message.
    CompileError,
    /// A match under the component-local wildcard policy, where an ordinary
    /// wildcard stays inside one path component and only `**` crosses a
    /// separator. This is the filesystem-glob reading, not the fnmatch one.
    MatchGlobPath,
}

/// One `.gitignore` file below the repository root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NestedIgnoreFile {
    /// Directory holding the file, relative to the root and without a
    /// trailing slash.
    pub directory: String,
    /// The file's rules, in order.
    pub rules: Vec<String>,
}

/// The path-separator platform a case is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Only `/` separates path components.
    Posix,
    /// Both `/` and `\\` separate path components.
    Windows,
}

impl Platform {
    /// Whether this platform is the one the current build targets.
    #[must_use]
    pub const fn is_host(self) -> bool {
        match self {
            Self::Posix => !cfg!(windows),
            Self::Windows => cfg!(windows),
        }
    }
}

impl Case {
    /// Whether the current host must replay this case.
    ///
    /// A case without a [`Platform`] runs everywhere; a platform-specific one
    /// is skipped on every other host, where its verdict does not hold.
    #[must_use]
    pub fn runs_on_host(&self) -> bool {
        match self.platform {
            None => true,
            Some(platform) => platform.is_host(),
        }
    }
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
    use super::{Case, CaseKind, Platform, decode_bytes, encode_bytes};

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

    #[test]
    fn missing_kind_defaults_to_matcher_for_existing_corpora() {
        let case: Case = serde_json::from_str(
            r#"{"id":"legacy","pattern":"*.rs","path":"lib.rs","expected":true,"source":"handwritten"}"#,
        )
        .unwrap();

        assert_eq!(case.kind, CaseKind::Matcher);
        assert_eq!(case.platform, None);
        assert!(case.runs_on_host());
        assert_eq!(case.error_offset, None);
        assert_eq!(case.error_message, None);
    }

    #[test]
    fn compile_error_cases_carry_an_optional_offset_and_message() {
        let case: Case = serde_json::from_str(
            r#"{"id":"error-unclosed","kind":"compile_error","pattern":"[abc","path":"","expected":false,"error_offset":0,"error_message":"unclosed character class","source":"handwritten"}"#,
        )
        .unwrap();

        assert_eq!(case.kind, CaseKind::CompileError);
        assert_eq!(case.error_offset, Some(0));
        assert_eq!(
            case.error_message.as_deref(),
            Some("unclosed character class")
        );
        assert!(case.runs_on_host());
    }

    #[test]
    fn platform_cases_run_only_on_their_own_host() {
        let windows: Case = serde_json::from_str(
            r#"{"id":"sep-windows","pattern":"a/b","path":"a\\x5Cb","expected":true,"platform":"windows","source":"handwritten"}"#,
        )
        .unwrap();
        let posix = Case {
            id: "sep-posix".to_owned(),
            platform: Some(Platform::Posix),
            expected: false,
            ..windows.clone()
        };

        assert_eq!(windows.platform, Some(Platform::Windows));
        assert_eq!(decode_bytes(&windows.path).unwrap(), b"a\\b");
        assert_eq!(windows.runs_on_host(), cfg!(windows));
        assert_eq!(posix.runs_on_host(), !cfg!(windows));
        assert!(windows.runs_on_host() != posix.runs_on_host());
    }

    #[test]
    fn optional_schema_fields_stay_absent_when_unused() {
        let case: Case = serde_json::from_str(
            r#"{"id":"legacy","pattern":"*.rs","path":"lib.rs","expected":true,"source":"handwritten"}"#,
        )
        .unwrap();

        let encoded = serde_json::to_string(&case).unwrap();
        assert!(!encoded.contains("platform"), "{encoded}");
        assert!(!encoded.contains("error_offset"), "{encoded}");
        assert!(!encoded.contains("error_message"), "{encoded}");
        assert!(!encoded.contains("nested_ignore_rules"), "{encoded}");
    }

    #[test]
    fn glob_path_cases_name_the_component_local_reading() {
        let case: Case = serde_json::from_str(
            r#"{"id":"fastglob-028","kind":"match_glob_path","pattern":"src/*.rs","path":"src/deep/main.rs","expected":false,"source":"fast_glob"}"#,
        )
        .unwrap();

        assert_eq!(case.kind, CaseKind::MatchGlobPath);
        assert!(case.nested_ignore_rules.is_empty());
    }

    #[test]
    fn nested_ignore_files_carry_their_directory() {
        let case: Case = serde_json::from_str(
            r#"{"id":"ignore-014","pattern":"!keep.log","path":"sub/keep.log","ignore_rules":["*.log"],"nested_ignore_rules":[{"directory":"sub","rules":["!keep.log"]}],"expected":false,"source":"git_check_ignore"}"#,
        )
        .unwrap();

        let [nested] = case.nested_ignore_rules.as_slice() else {
            panic!("expected exactly one nested ignore file");
        };
        assert_eq!(nested.directory, "sub");
        assert_eq!(nested.rules, ["!keep.log"]);
    }
}
