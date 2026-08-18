#![forbid(unsafe_code)]
#![doc = "Portable, byte-first glob matching."]

//! Compiled, byte-first glob patterns with explicit behaviour-changing options.
//!
//! The M1 implementation currently covers literals, `*`, `?`, `**`, character
//! classes, escapes, leading-period handling, ASCII case folding, and nested
//! brace expansion. Extglobs remain deliberately unimplemented.

use std::{collections::HashSet, error::Error, fmt};

/// A compiled glob pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    alternatives: Vec<Vec<Token>>,
    options: PatternOptions,
}

/// Explicit switches that affect glob interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternOptions {
    braces: bool,
    recursive_double_star: bool,
    extglob: bool,
    match_hidden: bool,
    case_insensitive: bool,
    escape: bool,
}

impl Default for PatternOptions {
    fn default() -> Self {
        Self {
            braces: false,
            recursive_double_star: false,
            extglob: false,
            match_hidden: false,
            case_insensitive: false,
            escape: true,
        }
    }
}

impl PatternOptions {
    /// Enables nested brace alternatives.
    #[must_use]
    pub const fn braces(mut self, enabled: bool) -> Self {
        self.braces = enabled;
        self
    }

    /// Gives a consecutive `**` recursive, separator-crossing semantics.
    #[must_use]
    pub const fn recursive_double_star(mut self, enabled: bool) -> Self {
        self.recursive_double_star = enabled;
        self
    }

    /// Enables Bash-style extglobs. Parsing is added in a later M1 step.
    #[must_use]
    pub const fn extglob(mut self, enabled: bool) -> Self {
        self.extglob = enabled;
        self
    }

    /// Allows wildcard tokens to match a leading period in a path component.
    #[must_use]
    pub const fn match_hidden(mut self, enabled: bool) -> Self {
        self.match_hidden = enabled;
        self
    }

    /// Performs ASCII-only case-insensitive matching.
    #[must_use]
    pub const fn case_insensitive(mut self, enabled: bool) -> Self {
        self.case_insensitive = enabled;
        self
    }

    /// Enables backslash escapes in patterns.
    #[must_use]
    pub const fn escape(mut self, enabled: bool) -> Self {
        self.escape = enabled;
        self
    }
}

/// An error returned when a pattern is not syntactically valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    offset: usize,
    message: &'static str,
}

impl PatternError {
    /// Zero-based byte offset of the invalid construct.
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Stable description of the invalid construct.
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.offset)
    }
}

impl Error for PatternError {}

impl Pattern {
    /// Compiles a pattern once for repeated matching against raw path bytes.
    pub fn compile(
        pattern: impl AsRef<[u8]>,
        options: PatternOptions,
    ) -> Result<Self, PatternError> {
        let pattern = pattern.as_ref();
        if options.braces {
            let parse_options = PatternOptions {
                braces: false,
                ..options
            };
            let alternatives = expand_braces(pattern, options.escape)?
                .into_iter()
                .map(|alternative| {
                    Self::compile(alternative, parse_options).map(|pattern| pattern.alternatives)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            return Ok(Self {
                alternatives,
                options,
            });
        }
        if options.extglob
            && pattern
                .windows(2)
                .any(|pair| matches!(pair[0], b'@' | b'!' | b'?' | b'*' | b'+') && pair[1] == b'(')
        {
            return Err(PatternError {
                offset: pattern
                    .windows(2)
                    .position(|pair| {
                        matches!(pair[0], b'@' | b'!' | b'?' | b'*' | b'+') && pair[1] == b'('
                    })
                    .expect("the checked pattern contains an extglob"),
                message: "extglob is not implemented",
            });
        }
        let mut tokens = Vec::new();
        let mut literals = Vec::new();
        let mut index = 0;

        while index < pattern.len() {
            match pattern[index] {
                b'/' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Separator);
                    index += 1;
                }
                b'*' if options.recursive_double_star && pattern.get(index + 1) == Some(&b'*') => {
                    flush_literals(&mut tokens, &mut literals);
                    if pattern.get(index + 2) == Some(&b'/') {
                        tokens.push(Token::RecursivePrefix);
                        index += 3;
                    } else {
                        tokens.push(Token::RecursiveStar);
                        index += 2;
                    }
                }
                b'*' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Star);
                    index += 1;
                }
                b'?' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Any);
                    index += 1;
                }
                b'[' => {
                    flush_literals(&mut tokens, &mut literals);
                    let (class, next) = parse_class(pattern, index, options.escape)?;
                    tokens.push(Token::Class(class));
                    index = next;
                }
                b'\\' if options.escape => {
                    if let Some(&escaped) = pattern.get(index + 1) {
                        literals.push(escaped);
                        index += 2;
                    } else {
                        // zlob's fnmatch core treats a trailing backslash as
                        // a literal backslash instead of rejecting the pattern.
                        literals.push(b'\\');
                        index += 1;
                    }
                }
                byte => {
                    literals.push(byte);
                    index += 1;
                }
            }
        }
        flush_literals(&mut tokens, &mut literals);

        Ok(Self {
            alternatives: vec![tokens],
            options,
        })
    }

    /// Reports whether a pattern is syntactically valid without retaining it.
    pub fn validate(
        pattern: impl AsRef<[u8]>,
        options: PatternOptions,
    ) -> Result<(), PatternError> {
        Self::compile(pattern, options).map(|_| ())
    }

    /// Matches the entire candidate path.
    #[must_use]
    pub fn is_match(&self, path: impl AsRef<[u8]>) -> bool {
        let path = path.as_ref();
        self.alternatives.iter().any(|tokens| {
            let mut failed = HashSet::new();
            self.matches_from(tokens, 0, 0, path, &mut failed)
        })
    }

    fn matches_from(
        &self,
        tokens: &[Token],
        token_index: usize,
        path_index: usize,
        path: &[u8],
        failed: &mut HashSet<(usize, usize)>,
    ) -> bool {
        if token_index == tokens.len() {
            return path_index == path.len();
        }
        if !failed.insert((token_index, path_index)) {
            return false;
        }

        let matches = match &tokens[token_index] {
            Token::Literal(literal) => {
                self.match_literal(tokens, token_index, path_index, path, failed, literal)
            }
            Token::Separator => {
                path.get(path_index).is_some_and(|byte| is_separator(*byte))
                    && self.matches_from(tokens, token_index + 1, path_index + 1, path, failed)
            }
            Token::Any => self.match_one(tokens, token_index, path_index, path, failed, |_| true),
            Token::Class(class) => {
                self.match_one(tokens, token_index, path_index, path, failed, |byte| {
                    class.matches(byte, self.options.case_insensitive)
                })
            }
            Token::Star => self.match_star(tokens, token_index, path_index, path, failed),
            Token::RecursiveStar | Token::RecursivePrefix => {
                self.match_star(tokens, token_index, path_index, path, failed)
            }
        };

        if matches {
            failed.remove(&(token_index, path_index));
        }
        matches
    }

    fn match_literal(
        &self,
        tokens: &[Token],
        token_index: usize,
        path_index: usize,
        path: &[u8],
        failed: &mut HashSet<(usize, usize)>,
        literal: &[u8],
    ) -> bool {
        let Some(candidate) = path.get(path_index..path_index + literal.len()) else {
            return false;
        };
        if literal.iter().zip(candidate).all(|(&expected, &actual)| {
            bytes_equal(expected, actual, self.options.case_insensitive)
        }) {
            self.matches_from(
                tokens,
                token_index + 1,
                path_index + literal.len(),
                path,
                failed,
            )
        } else {
            false
        }
    }

    fn match_one(
        &self,
        tokens: &[Token],
        token_index: usize,
        path_index: usize,
        path: &[u8],
        failed: &mut HashSet<(usize, usize)>,
        accepts: impl FnOnce(u8) -> bool,
    ) -> bool {
        path.get(path_index).is_some_and(|&byte| {
            accepts(byte)
                && (self.options.match_hidden
                    || byte != b'.'
                    || !at_component_start(path, path_index))
                && self.matches_from(tokens, token_index + 1, path_index + 1, path, failed)
        })
    }

    fn match_star(
        &self,
        tokens: &[Token],
        token_index: usize,
        path_index: usize,
        path: &[u8],
        failed: &mut HashSet<(usize, usize)>,
    ) -> bool {
        if self.matches_from(tokens, token_index + 1, path_index, path, failed) {
            return true;
        }
        let Some(&byte) = path.get(path_index) else {
            return false;
        };
        if !self.options.match_hidden && byte == b'.' && at_component_start(path, path_index) {
            return false;
        }
        self.matches_from(tokens, token_index, path_index + 1, path, failed)
    }
}

/// Crate version exposed for build and integration diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(Vec<u8>),
    Separator,
    Any,
    Star,
    RecursiveStar,
    RecursivePrefix,
    Class(Class),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Class {
    negated: bool,
    members: Vec<ClassMember>,
}

impl Class {
    fn matches(&self, byte: u8, case_insensitive: bool) -> bool {
        let included = self.members.iter().any(|member| match member {
            ClassMember::Byte(expected) => bytes_equal(*expected, byte, case_insensitive),
            ClassMember::Range(start, end) => {
                let byte = fold_ascii(byte, case_insensitive);
                let start = fold_ascii(*start, case_insensitive);
                let end = fold_ascii(*end, case_insensitive);
                start <= byte && byte <= end
            }
        });
        included != self.negated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassMember {
    Byte(u8),
    Range(u8, u8),
}

fn parse_class(
    pattern: &[u8],
    start: usize,
    escapes: bool,
) -> Result<(Class, usize), PatternError> {
    let mut index = start + 1;
    let mut negated = false;
    if pattern
        .get(index)
        .is_some_and(|byte| matches!(byte, b'!' | b'^'))
    {
        negated = true;
        index += 1;
    }

    let mut values = Vec::new();
    if pattern.get(index) == Some(&b']') {
        values.push(b']');
        index += 1;
    }
    while let Some(&byte) = pattern.get(index) {
        if byte == b']' {
            if values.is_empty() {
                return Err(PatternError {
                    offset: start,
                    message: "empty character class",
                });
            }
            return Ok((
                Class {
                    negated,
                    members: class_members(values),
                },
                index + 1,
            ));
        }
        if byte == b'\\' && escapes {
            let Some(&escaped) = pattern.get(index + 1) else {
                return Err(PatternError {
                    offset: index,
                    message: "trailing escape in character class",
                });
            };
            values.push(escaped);
            index += 2;
        } else {
            values.push(byte);
            index += 1;
        }
    }

    Err(PatternError {
        offset: start,
        message: "unclosed character class",
    })
}

fn class_members(values: Vec<u8>) -> Vec<ClassMember> {
    let mut members = Vec::new();
    let mut index = 0;
    while index < values.len() {
        if index + 2 < values.len() && values[index + 1] == b'-' {
            members.push(ClassMember::Range(values[index], values[index + 2]));
            index += 3;
        } else {
            members.push(ClassMember::Byte(values[index]));
            index += 1;
        }
    }
    members
}

fn flush_literals(tokens: &mut Vec<Token>, literals: &mut Vec<u8>) {
    if !literals.is_empty() {
        tokens.push(Token::Literal(std::mem::take(literals)));
    }
}

fn expand_braces(pattern: &[u8], escapes: bool) -> Result<Vec<Vec<u8>>, PatternError> {
    let Some(open) = first_unescaped_brace(pattern, escapes) else {
        return Ok(vec![pattern.to_vec()]);
    };
    let Some(close) = matching_brace(pattern, open, escapes) else {
        // zlob treats an unmatched brace as ordinary text.
        return Ok(vec![pattern.to_vec()]);
    };

    let alternatives = split_brace_alternatives(&pattern[open + 1..close], escapes);
    let mut expanded = Vec::new();
    for alternative in alternatives {
        let mut combined = Vec::with_capacity(open + alternative.len() + pattern.len() - close - 1);
        combined.extend_from_slice(&pattern[..open]);
        combined.extend_from_slice(alternative);
        combined.extend_from_slice(&pattern[close + 1..]);
        expanded.extend(expand_braces(&combined, escapes)?);
    }
    Ok(expanded)
}

fn first_unescaped_brace(pattern: &[u8], escapes: bool) -> Option<usize> {
    let mut index = 0;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
        } else if pattern[index] == b'{' {
            return Some(index);
        } else {
            index += 1;
        }
    }
    None
}

fn matching_brace(pattern: &[u8], open: usize, escapes: bool) -> Option<usize> {
    let mut depth = 0_usize;
    let mut index = open;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        match pattern[index] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn split_brace_alternatives(content: &[u8], escapes: bool) -> Vec<&[u8]> {
    let mut alternatives = Vec::new();
    let mut start = 0;
    let mut depth = 0_usize;
    let mut index = 0;
    while index < content.len() {
        if escapes && content[index] == b'\\' {
            index += 2;
            continue;
        }
        match content[index] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
                alternatives.push(&content[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    alternatives.push(&content[start..]);
    alternatives
}

fn is_separator(byte: u8) -> bool {
    byte == b'/' || (cfg!(windows) && byte == b'\\')
}

fn at_component_start(path: &[u8], index: usize) -> bool {
    index == 0 || path.get(index - 1).is_some_and(|byte| is_separator(*byte))
}

fn bytes_equal(expected: u8, actual: u8, case_insensitive: bool) -> bool {
    fold_ascii(expected, case_insensitive) == fold_ascii(actual, case_insensitive)
}

fn fold_ascii(byte: u8, case_insensitive: bool) -> u8 {
    if case_insensitive {
        byte.to_ascii_lowercase()
    } else {
        byte
    }
}

#[cfg(test)]
mod tests {
    use super::{Pattern, PatternOptions};

    fn compile(pattern: &str) -> Pattern {
        Pattern::compile(pattern, PatternOptions::default()).unwrap()
    }

    #[test]
    fn matches_literals_and_component_wildcards() {
        assert!(compile("src/*.rs").is_match("src/lib.rs"));
        assert!(compile("src/?.rs").is_match("src/a.rs"));
        assert!(compile("src/*.rs").is_match("src/bin/main.rs"));
        assert!(compile("*.rs").is_match("lib.rs"));
    }

    #[test]
    fn leading_period_requires_an_explicit_option_or_literal() {
        assert!(!compile("*").is_match(".gitignore"));
        assert!(compile(".*").is_match(".gitignore"));
        assert!(
            Pattern::compile("*", PatternOptions::default().match_hidden(true))
                .unwrap()
                .is_match(".gitignore")
        );
    }

    #[test]
    fn recursive_double_star_is_explicit_and_component_aware() {
        let options = PatternOptions::default().recursive_double_star(true);
        let pattern = Pattern::compile("**/*.rs", options).unwrap();
        assert!(pattern.is_match("lib.rs"));
        assert!(pattern.is_match("src/bin/main.rs"));
        assert!(!pattern.is_match("src/.private.rs"));
        assert!(compile("**/*.rs").is_match("src/main.rs"));
        assert!(compile("**/*.rs").is_match("src/bin/main.rs"));
    }

    #[test]
    fn character_classes_support_ranges_and_negation() {
        assert!(compile("file[0-9].rs").is_match("file7.rs"));
        assert!(compile("file[!0-9].rs").is_match("filex.rs"));
        assert!(compile("file[^0-9].rs").is_match("filex.rs"));
        assert!(!compile("file[!0-9].rs").is_match("file7.rs"));
    }

    #[test]
    fn braces_expand_nested_and_empty_alternatives() {
        let options = PatternOptions::default().braces(true);
        let pattern = Pattern::compile("{src,{lib,bin}}/*.{rs,toml}", options).unwrap();
        assert!(pattern.is_match("src/main.rs"));
        assert!(pattern.is_match("lib/Cargo.toml"));
        assert!(pattern.is_match("bin/main.rs"));
        assert!(!pattern.is_match("tests/main.rs"));
        assert!(
            Pattern::compile("test{,_suffix}.txt", options)
                .unwrap()
                .is_match("test_suffix.txt")
        );
    }

    #[test]
    fn escapes_and_case_folding_are_opt_in_and_byte_oriented() {
        assert!(compile("\\*.rs").is_match("*.rs"));
        assert!(!compile("\\*.rs").is_match("main.rs"));
        assert!(
            Pattern::compile(
                "README.md",
                PatternOptions::default().case_insensitive(true)
            )
            .unwrap()
            .is_match("readme.MD")
        );
    }

    #[test]
    fn invalid_syntax_has_a_location() {
        let error = Pattern::compile("[abc", PatternOptions::default()).unwrap_err();
        assert_eq!(error.offset(), 0);
        assert_eq!(error.message(), "unclosed character class");
        assert!(compile("foo\\").is_match("foo\\"));
    }
}
