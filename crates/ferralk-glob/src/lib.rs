#![forbid(unsafe_code)]
#![doc = "Portable, byte-first glob matching."]

//! Compiled, byte-first glob patterns with explicit behaviour-changing options.
//!
//! The M1 implementation currently covers literals, `*`, `?`, `**`, character
//! classes, escapes, leading-period handling, ASCII case folding, nested brace
//! expansion, and Bash-style extglobs.
//!
//! Provenance: semantics are ported and differentially checked against zlob
//! v1.6.3, source commit 4bc4da2cbc823d3911b4a1436448687c398977dd, primarily
//! `zig-src/fnmatch.zig`, `zig-src/pattern_context.zig`, and
//! `test/test_fnmatch.zig`. Deliberate differences live in
//! the checked-in corpus and compatibility matrix.

use std::{collections::HashSet, error::Error, fmt};

use memchr::{memchr, memchr3, memmem};

/// A compiled glob pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    alternatives: Vec<Alternative>,
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
    component_wildcards: bool,
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
            component_wildcards: false,
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

    /// Enables Bash-style extglobs.
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
    /// Reports whether the enabled syntax options make a pattern non-literal.
    ///
    /// This is a byte-first preflight helper. Like zlob's `hasWildcards`, it
    /// deliberately reports syntax markers even when an escape option would
    /// later make a particular marker literal.
    #[must_use]
    pub fn has_wildcards(pattern: impl AsRef<[u8]>, options: PatternOptions) -> bool {
        let pattern = pattern.as_ref();
        if memchr3(b'*', b'?', b'[', pattern).is_some() {
            return true;
        }
        if options.braces && memchr(b'{', pattern).is_some() {
            return true;
        }
        if !options.extglob {
            return false;
        }
        let mut offset = 0;
        while let Some(found) = memchr(b'(', &pattern[offset..]) {
            let index = offset + found;
            if index > 0 && matches!(pattern[index - 1], b'?' | b'*' | b'+' | b'@' | b'!') {
                return true;
            }
            offset = index + 1;
        }
        false
    }

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
            alternatives: vec![Alternative {
                raw: pattern.to_vec(),
                fast_path: FastPath::compile(&tokens, options),
                tokens,
            }],
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
        self.alternatives.iter().any(|alternative| {
            if self.options.extglob && contains_extglob(&alternative.raw, self.options.escape) {
                match_extglob(&alternative.raw, path, self.options)
            } else if let Some(fast_path) = &alternative.fast_path {
                fast_path.is_match(path, self.options)
            } else {
                let mut failed = FailedStates::new(&alternative.tokens, path);
                self.matches_from(&alternative.tokens, 0, 0, path, &mut failed)
            }
        })
    }

    /// Returns the input paths accepted by this compiled pattern, in input
    /// order. Wildcards after an explicit separator stay within that path
    /// component; recursive `**` is the separator-crossing form. The returned
    /// references borrow the caller-owned path list.
    #[must_use]
    pub fn filter_paths<'a, T>(&self, paths: impl IntoIterator<Item = &'a T>) -> Vec<&'a T>
    where
        T: AsRef<[u8]> + ?Sized + 'a,
    {
        if !self.needs_path_filter_semantics() {
            return paths
                .into_iter()
                .filter(|path| self.is_match(path.as_ref()))
                .collect();
        }
        let mut matcher = self.clone();
        matcher.options.component_wildcards = true;
        for alternative in &mut matcher.alternatives {
            alternative.fast_path = None;
            let leading_dot_slash = alternative.raw.starts_with(b"./")
                && matches!(alternative.tokens.as_slice(), [Token::Literal(dot), Token::Separator, ..] if dot == b".");
            if leading_dot_slash {
                alternative.raw.drain(..2);
                alternative.tokens.drain(..2);
            }
        }
        paths
            .into_iter()
            .filter(|path| matcher.is_match(path.as_ref()))
            .collect()
    }

    fn needs_path_filter_semantics(&self) -> bool {
        self.alternatives.iter().any(|alternative| {
            alternative.raw.starts_with(b"./")
                || alternative.tokens.windows(2).any(|tokens| {
                    matches!(
                        tokens,
                        [Token::Separator, Token::Any | Token::Star | Token::Class(_)]
                    )
                })
        })
    }

    fn matches_from(
        &self,
        tokens: &[Token],
        token_index: usize,
        path_index: usize,
        path: &[u8],
        failed: &mut FailedStates,
    ) -> bool {
        if token_index == tokens.len() {
            return path_index == path.len();
        }
        if !failed.insert(token_index, path_index) {
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
            Token::Star => self.match_star(
                tokens,
                token_index,
                path_index,
                path,
                failed,
                !self.component_wildcard(tokens, token_index),
            ),
            Token::RecursiveStar | Token::RecursivePrefix => {
                self.match_star(tokens, token_index, path_index, path, failed, true)
            }
        };

        if matches {
            failed.remove(token_index, path_index);
        }
        matches
    }

    fn component_wildcard(&self, tokens: &[Token], token_index: usize) -> bool {
        self.options.component_wildcards
            && token_index > 0
            && matches!(tokens[token_index - 1], Token::Separator)
    }

    fn match_literal(
        &self,
        tokens: &[Token],
        token_index: usize,
        path_index: usize,
        path: &[u8],
        failed: &mut FailedStates,
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
        failed: &mut FailedStates,
        accepts: impl FnOnce(u8) -> bool,
    ) -> bool {
        path.get(path_index).is_some_and(|&byte| {
            accepts(byte)
                && (!self.component_wildcard(tokens, token_index) || !is_separator(byte))
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
        failed: &mut FailedStates,
        recursive: bool,
    ) -> bool {
        if self.matches_from(tokens, token_index + 1, path_index, path, failed) {
            return true;
        }
        let Some(&byte) = path.get(path_index) else {
            return false;
        };
        if !recursive && self.options.component_wildcards && is_separator(byte) {
            return false;
        }
        if !self.options.match_hidden && byte == b'.' && at_component_start(path, path_index) {
            return false;
        }
        self.matches_from(tokens, token_index, path_index + 1, path, failed)
    }
}

/// Memoized failed token/path pairs for one matcher invocation.
///
/// The state space is dense by construction: token and path indices are both
/// bounded by the input slices. A flat matrix avoids hashing and allocation per
/// recursive probe while keeping the original backtracking semantics.
struct FailedStates {
    width: usize,
    states: Vec<bool>,
}

impl FailedStates {
    fn new(tokens: &[Token], path: &[u8]) -> Self {
        let width = path.len() + 1;
        Self {
            width,
            states: vec![false; tokens.len() * width],
        }
    }

    fn insert(&mut self, token_index: usize, path_index: usize) -> bool {
        let state = &mut self.states[token_index * self.width + path_index];
        if *state {
            false
        } else {
            *state = true;
            true
        }
    }

    fn remove(&mut self, token_index: usize, path_index: usize) {
        self.states[token_index * self.width + path_index] = false;
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
struct Alternative {
    raw: Vec<u8>,
    fast_path: Option<FastPath>,
    tokens: Vec<Token>,
}

/// An allocation-free matcher for a common recursive-prefix/suffix shape.
///
/// It is deliberately narrower than the full token matcher. Keeping it as a
/// compiled variant makes the optimization opt-in by syntax and leaves every
/// other pattern on the corpus-tested general path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FastPath {
    LiteralTokens(Vec<Token>),
    Star,
    PrefixStar { prefix: Vec<u8> },
    StarSuffix { suffix: Vec<u8> },
    RecursivePrefixSuffix { prefix: Vec<u8>, suffix: Vec<u8> },
}

impl FastPath {
    fn compile(tokens: &[Token], options: PatternOptions) -> Option<Self> {
        if options.component_wildcards {
            return None;
        }
        if tokens
            .iter()
            .all(|token| matches!(token, Token::Literal(_) | Token::Separator))
        {
            return Some(Self::LiteralTokens(tokens.to_vec()));
        }
        match tokens {
            [Token::Star] => return Some(Self::Star),
            [Token::Literal(prefix), Token::Star] => {
                return Some(Self::PrefixStar {
                    prefix: prefix.clone(),
                });
            }
            [Token::Star, Token::Literal(suffix)] => {
                return Some(Self::StarSuffix {
                    suffix: suffix.clone(),
                });
            }
            _ => {}
        }
        if options.case_insensitive {
            return None;
        }
        let [
            Token::Literal(prefix),
            Token::Separator,
            Token::RecursivePrefix,
            Token::Star,
            Token::Literal(suffix),
        ] = tokens
        else {
            return None;
        };
        Some(Self::RecursivePrefixSuffix {
            prefix: prefix.clone(),
            suffix: suffix.clone(),
        })
    }

    fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        match self {
            Self::LiteralTokens(tokens) => {
                let mut path_index = 0;
                for token in tokens {
                    match token {
                        Token::Literal(literal) => {
                            let Some(candidate) = path.get(path_index..path_index + literal.len())
                            else {
                                return false;
                            };
                            if !literal.iter().zip(candidate).all(|(&expected, &actual)| {
                                bytes_equal(expected, actual, options.case_insensitive)
                            }) {
                                return false;
                            }
                            path_index += literal.len();
                        }
                        Token::Separator => {
                            if !path.get(path_index).is_some_and(|byte| is_separator(*byte)) {
                                return false;
                            }
                            path_index += 1;
                        }
                        _ => unreachable!("literal fast path only stores literal tokens"),
                    }
                }
                path_index == path.len()
            }
            Self::Star => options.match_hidden || !contains_hidden_component(path),
            Self::PrefixStar { prefix } => {
                let Some(variable) = strip_literal_prefix(path, prefix, options.case_insensitive)
                else {
                    return false;
                };
                options.match_hidden || !contains_hidden_component(variable)
            }
            Self::StarSuffix { suffix } => {
                let Some(variable) = strip_literal_suffix(path, suffix, options.case_insensitive)
                else {
                    return false;
                };
                options.match_hidden || !contains_hidden_component(variable)
            }
            Self::RecursivePrefixSuffix { prefix, suffix } => {
                let Some(remainder) = path
                    .strip_prefix(prefix.as_slice())
                    .and_then(|path| path.strip_prefix(b"/"))
                else {
                    return false;
                };
                let Some(variable) = remainder.strip_suffix(suffix.as_slice()) else {
                    return false;
                };
                options.match_hidden || !contains_hidden_component(variable)
            }
        }
    }
}

fn strip_literal_prefix<'a>(
    path: &'a [u8],
    literal: &[u8],
    case_insensitive: bool,
) -> Option<&'a [u8]> {
    let candidate = path.get(..literal.len())?;
    literal
        .iter()
        .zip(candidate)
        .all(|(&expected, &actual)| bytes_equal(expected, actual, case_insensitive))
        .then_some(&path[literal.len()..])
}

fn strip_literal_suffix<'a>(
    path: &'a [u8],
    literal: &[u8],
    case_insensitive: bool,
) -> Option<&'a [u8]> {
    let start = path.len().checked_sub(literal.len())?;
    literal
        .iter()
        .zip(&path[start..])
        .all(|(&expected, &actual)| bytes_equal(expected, actual, case_insensitive))
        .then_some(&path[..start])
}

fn contains_hidden_component(path: &[u8]) -> bool {
    let mut offset = 0;
    while let Some(found) = memchr(b'.', &path[offset..]) {
        let index = offset + found;
        if index == 0 || is_separator(path[index - 1]) {
            return true;
        }
        offset = index + 1;
    }
    false
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
            ClassMember::Posix(class) => class.matches(byte, case_insensitive),
        });
        included != self.negated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassMember {
    Byte(u8),
    Range(u8, u8),
    Posix(PosixClass),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PosixClass {
    Alnum,
    Alpha,
    Ascii,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Word,
    Xdigit,
}

impl PosixClass {
    fn matches(self, byte: u8, case_insensitive: bool) -> bool {
        let folded = fold_ascii(byte, case_insensitive);
        match self {
            Self::Alnum => folded.is_ascii_alphanumeric(),
            Self::Alpha => folded.is_ascii_alphabetic(),
            Self::Ascii => byte.is_ascii(),
            Self::Blank => matches!(byte, b' ' | b'\t'),
            Self::Cntrl => byte.is_ascii_control(),
            Self::Digit => byte.is_ascii_digit(),
            Self::Graph => byte.is_ascii_graphic(),
            Self::Lower => folded.is_ascii_lowercase(),
            Self::Print => byte.is_ascii_graphic() || byte == b' ',
            Self::Punct => byte.is_ascii_punctuation(),
            Self::Space => byte.is_ascii_whitespace(),
            Self::Upper => folded.is_ascii_uppercase(),
            Self::Word => folded.is_ascii_alphanumeric() || folded == b'_',
            Self::Xdigit => byte.is_ascii_hexdigit(),
        }
    }
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
    let mut members = Vec::new();
    if pattern.get(index) == Some(&b']') {
        values.push(b']');
        index += 1;
    }
    while let Some(&byte) = pattern.get(index) {
        if byte == b']' {
            if values.is_empty() && members.is_empty() {
                return Err(PatternError {
                    offset: start,
                    message: "empty character class",
                });
            }
            return Ok((
                Class {
                    negated,
                    members: {
                        members.extend(class_members(values));
                        members
                    },
                },
                index + 1,
            ));
        }
        if byte == b'['
            && pattern.get(index + 1) == Some(&b':')
            && let Some(end) = memmem::find(&pattern[index + 2..], b":]")
            && let Some(class) = parse_posix_class(&pattern[index + 2..index + 2 + end])
        {
            let name_end = index + 2 + end;
            members.extend(class_members(std::mem::take(&mut values)));
            members.push(ClassMember::Posix(class));
            index = name_end + 2;
            continue;
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

fn parse_posix_class(name: &[u8]) -> Option<PosixClass> {
    match name {
        b"alnum" => Some(PosixClass::Alnum),
        b"alpha" => Some(PosixClass::Alpha),
        b"ascii" => Some(PosixClass::Ascii),
        b"blank" => Some(PosixClass::Blank),
        b"cntrl" => Some(PosixClass::Cntrl),
        b"digit" => Some(PosixClass::Digit),
        b"graph" => Some(PosixClass::Graph),
        b"lower" => Some(PosixClass::Lower),
        b"print" => Some(PosixClass::Print),
        b"punct" => Some(PosixClass::Punct),
        b"space" => Some(PosixClass::Space),
        b"upper" => Some(PosixClass::Upper),
        b"word" => Some(PosixClass::Word),
        b"xdigit" => Some(PosixClass::Xdigit),
        _ => None,
    }
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
    if !escapes {
        return memchr(b'{', pattern);
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtglobKind {
    Optional,
    ZeroOrMore,
    OneOrMore,
    ExactlyOne,
    Negated,
}

fn contains_extglob(pattern: &[u8], escapes: bool) -> bool {
    let mut index = 0;
    while index + 1 < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        if detect_extglob_at(pattern, index).is_some() {
            return true;
        }
        index += 1;
    }
    false
}

fn detect_extglob_at(pattern: &[u8], index: usize) -> Option<ExtglobKind> {
    if pattern.get(index + 1) != Some(&b'(') {
        return None;
    }
    match pattern[index] {
        b'?' => Some(ExtglobKind::Optional),
        b'*' => Some(ExtglobKind::ZeroOrMore),
        b'+' => Some(ExtglobKind::OneOrMore),
        b'@' => Some(ExtglobKind::ExactlyOne),
        b'!' => Some(ExtglobKind::Negated),
        _ => None,
    }
}

fn closing_extglob_parenthesis(pattern: &[u8], open: usize, escapes: bool) -> Option<usize> {
    if pattern.get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 1_usize;
    let mut index = open + 1;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        match pattern[index] {
            b'(' => depth += 1,
            b')' => {
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

fn split_extglob_alternatives(content: &[u8], escapes: bool) -> Vec<&[u8]> {
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
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b'|' if depth == 0 => {
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

fn match_extglob(pattern: &[u8], path: &[u8], options: PatternOptions) -> bool {
    match_extglob_from(pattern, path, 0, 0, options)
}

fn match_extglob_from(
    pattern: &[u8],
    path: &[u8],
    mut pattern_index: usize,
    mut path_index: usize,
    options: PatternOptions,
) -> bool {
    let mut star_pattern_index = 0_usize;
    let mut star_path_index = 0_usize;
    let mut has_star = false;

    while path_index < path.len() || pattern_index < pattern.len() {
        if pattern_index < pattern.len() {
            if let Some(kind) = detect_extglob_at(pattern, pattern_index) {
                let open = pattern_index + 1;
                if let Some(close) = closing_extglob_parenthesis(pattern, open, options.escape) {
                    let alternatives =
                        split_extglob_alternatives(&pattern[open + 1..close], options.escape);
                    if match_extglob_group(
                        kind,
                        &alternatives,
                        &pattern[close + 1..],
                        path,
                        path_index,
                        options,
                    ) {
                        return true;
                    }
                    if has_star && star_path_index < path.len() {
                        pattern_index = star_pattern_index;
                        star_path_index += 1;
                        path_index = star_path_index;
                        continue;
                    }
                    return false;
                }
            }

            match pattern[pattern_index] {
                b'*' => {
                    pattern_index += 1;
                    while pattern.get(pattern_index) == Some(&b'*') {
                        pattern_index += 1;
                    }
                    star_pattern_index = pattern_index;
                    star_path_index = path_index;
                    has_star = true;
                    continue;
                }
                b'?' if path.get(path_index).is_some_and(|&byte| {
                    options.match_hidden || byte != b'.' || !at_component_start(path, path_index)
                }) =>
                {
                    pattern_index += 1;
                    path_index += 1;
                    continue;
                }
                b'[' if let Ok((class, next)) =
                    parse_class(pattern, pattern_index, options.escape)
                    && path.get(path_index).is_some_and(|&byte| {
                        (options.match_hidden
                            || byte != b'.'
                            || !at_component_start(path, path_index))
                            && class.matches(byte, options.case_insensitive)
                    }) =>
                {
                    pattern_index = next;
                    path_index += 1;
                    continue;
                }
                b'\\'
                    if options.escape
                        && pattern_index + 1 < pattern.len()
                        && path.get(path_index).is_some_and(|&byte| {
                            bytes_equal(pattern[pattern_index + 1], byte, options.case_insensitive)
                        }) =>
                {
                    pattern_index += 2;
                    path_index += 1;
                    continue;
                }
                byte if path
                    .get(path_index)
                    .is_some_and(|&actual| bytes_equal(byte, actual, options.case_insensitive)) =>
                {
                    pattern_index += 1;
                    path_index += 1;
                    continue;
                }
                _ => {}
            }
        }

        if has_star && star_path_index < path.len() {
            pattern_index = star_pattern_index;
            star_path_index += 1;
            path_index = star_path_index;
            continue;
        }
        return false;
    }
    true
}

fn match_extglob_group(
    kind: ExtglobKind,
    alternatives: &[&[u8]],
    rest: &[u8],
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
) -> bool {
    match kind {
        ExtglobKind::ExactlyOne => {
            match_extglob_alternative(alternatives, rest, path, path_index, options)
        }
        ExtglobKind::Optional => {
            match_extglob_from(rest, path, 0, path_index, options)
                || match_extglob_alternative(alternatives, rest, path, path_index, options)
        }
        ExtglobKind::ZeroOrMore => {
            match_extglob_from(rest, path, 0, path_index, options)
                || match_extglob_repeated(
                    alternatives,
                    rest,
                    path,
                    path_index,
                    options,
                    &mut HashSet::new(),
                )
        }
        ExtglobKind::OneOrMore => match_extglob_repeated(
            alternatives,
            rest,
            path,
            path_index,
            options,
            &mut HashSet::new(),
        ),
        ExtglobKind::Negated => {
            for end in path_index..=path.len() {
                if alternatives.iter().all(|alternative| {
                    !match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                }) && match_extglob_from(rest, path, 0, end, options)
                {
                    return true;
                }
            }
            false
        }
    }
}

fn match_extglob_alternative(
    alternatives: &[&[u8]],
    rest: &[u8],
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
) -> bool {
    alternatives.iter().any(|alternative| {
        (path_index..=path.len()).any(|end| {
            match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                && match_extglob_from(rest, path, 0, end, options)
        })
    })
}

fn match_extglob_repeated(
    alternatives: &[&[u8]],
    rest: &[u8],
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    visited: &mut HashSet<usize>,
) -> bool {
    if !visited.insert(path_index) {
        return false;
    }
    for alternative in alternatives {
        for end in path_index..=path.len() {
            if match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                && (match_extglob_from(rest, path, 0, end, options)
                    || (end > path_index
                        && match_extglob_repeated(alternatives, rest, path, end, options, visited)))
            {
                return true;
            }
        }
    }
    false
}

fn match_extglob_alternative_exact(
    alternative: &[u8],
    path: &[u8],
    options: PatternOptions,
) -> bool {
    let options = PatternOptions {
        braces: false,
        extglob: false,
        ..options
    };
    Pattern::compile(alternative, options).is_ok_and(|pattern| pattern.is_match(path))
}

#[cfg(test)]
mod tests {
    use super::{FastPath, Pattern, PatternOptions};

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
    fn character_classes_support_posix_named_sets() {
        assert!(compile("[[:alpha:]]").is_match("a"));
        assert!(!compile("[[:alpha:]]").is_match("7"));
        assert!(compile("[[:digit:]]").is_match("7"));
        assert!(compile("[[:word:]]").is_match("_"));
        assert!(compile("[![:space:]]").is_match("x"));
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
    fn extglobs_support_alternation_repetition_and_negation() {
        let options = PatternOptions::default().extglob(true);
        assert!(
            Pattern::compile("@(foo|bar)", options)
                .unwrap()
                .is_match("foo")
        );
        assert!(
            Pattern::compile("file?(.bak).txt", options)
                .unwrap()
                .is_match("file.txt")
        );
        assert!(
            Pattern::compile("a*(X)b", options)
                .unwrap()
                .is_match("aXXXb")
        );
        assert!(
            Pattern::compile("+(a|b)", options)
                .unwrap()
                .is_match("abba")
        );
        assert!(
            Pattern::compile("*.!(js)", options)
                .unwrap()
                .is_match("file.txt")
        );
        assert!(
            !Pattern::compile("*.!(js)", options)
                .unwrap()
                .is_match("file.js")
        );
        assert!(
            !Pattern::compile("@(foo|bar)", PatternOptions::default())
                .unwrap()
                .is_match("foo")
        );
    }

    #[test]
    fn literal_only_patterns_match_exactly_over_exhaustive_byte_words() {
        let words = byte_words(b"abc", 3);
        for pattern in &words {
            let matcher = Pattern::compile(pattern, PatternOptions::default())
                .expect("literal byte patterns compile");
            for candidate in &words {
                assert_eq!(
                    matcher.is_match(candidate),
                    pattern == candidate,
                    "literal pattern {pattern:?} against {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn wildcard_subset_invariants_hold_over_exhaustive_byte_words() {
        let options = PatternOptions::default().match_hidden(true);
        let any = Pattern::compile("?", options).expect("single wildcard compiles");
        let star = Pattern::compile("*", options).expect("star wildcard compiles");
        let prefixed = Pattern::compile("a*", options).expect("prefixed star compiles");
        let words = byte_words(b"ab.", 4);
        for candidate in &words {
            assert!(
                !any.is_match(candidate) || star.is_match(candidate),
                "question-mark matches a path the star rejects: {candidate:?}"
            );
            assert!(
                !prefixed.is_match(candidate) || star.is_match(candidate),
                "prefixed star matches a path the unrestricted star rejects: {candidate:?}"
            );
        }
    }

    #[test]
    fn recursive_prefix_suffix_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().recursive_double_star(true);
        let fast = Pattern::compile("src/**/*.rs", options).expect("pattern compiles");
        assert!(fast.alternatives[0].fast_path.is_some());
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        let mut candidates = vec![
            b"src/.rs".to_vec(),
            b"src/.hidden.rs".to_vec(),
            b"src/visible.rs".to_vec(),
            b"src/nested/.hidden.rs".to_vec(),
            b"src/nested/visible.rs".to_vec(),
            b"other/visible.rs".to_vec(),
        ];
        candidates.extend(
            byte_words(b"ab./rs", 4)
                .into_iter()
                .map(|suffix| [b"src/".as_slice(), suffix.as_slice()].concat()),
        );
        for candidate in candidates {
            assert_eq!(
                fast.is_match(&candidate),
                general.is_match(&candidate),
                "fast path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn literal_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        let fast = Pattern::compile("Src/Readme", options).expect("pattern compiles");
        assert!(matches!(
            fast.alternatives[0].fast_path,
            Some(FastPath::LiteralTokens(_))
        ));
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        for candidate in [
            b"src/readme".as_slice(),
            b"SRC/README".as_slice(),
            b"src/readme.txt".as_slice(),
            b"src/readme/".as_slice(),
            b"other/readme".as_slice(),
        ] {
            assert_eq!(
                fast.is_match(candidate),
                general.is_match(candidate),
                "fast path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn single_star_fast_paths_match_the_general_matcher() {
        let candidates = byte_words(b"ab./rs", 4);
        for pattern in ["*", "src*", "*.rs"] {
            let fast =
                Pattern::compile(pattern, PatternOptions::default()).expect("pattern compiles");
            assert!(fast.alternatives[0].fast_path.is_some());
            let mut general = fast.clone();
            general.alternatives[0].fast_path = None;
            for candidate in &candidates {
                assert_eq!(
                    fast.is_match(candidate),
                    general.is_match(candidate),
                    "fast path differs for {pattern:?} against {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn filter_paths_preserves_input_order_and_borrows_inputs() {
        let pattern = Pattern::compile("*.txt", PatternOptions::default()).unwrap();
        let paths = ["first.txt", "skip.rs", "second.txt"];
        assert_eq!(
            pattern.filter_paths(&paths),
            vec![&"first.txt", &"second.txt"]
        );
    }

    #[test]
    fn filter_paths_keeps_wildcards_after_a_separator_in_one_component() {
        let pattern = Pattern::compile(
            "**/lua/*.lua",
            PatternOptions::default().recursive_double_star(true),
        )
        .unwrap();
        let paths = [
            "lua/init.lua",
            "nvim/lua/setup.lua",
            "nvim/lua/sub/nested.lua",
        ];
        assert_eq!(
            pattern.filter_paths(&paths),
            vec![&"lua/init.lua", &"nvim/lua/setup.lua"]
        );
        assert!(pattern.is_match("nvim/lua/sub/nested.lua"));
    }

    #[test]
    fn filter_paths_normalizes_a_leading_dot_slash() {
        let options = PatternOptions::default().recursive_double_star(true);
        let bare = Pattern::compile("**/*.lua", options).unwrap();
        let dotted = Pattern::compile("./**/*.lua", options).unwrap();
        let paths = [
            "init.lua",
            "lua/setup.lua",
            "nested/deep/plugin.lua",
            "src/main.zig",
        ];
        assert_eq!(dotted.filter_paths(&paths), bare.filter_paths(&paths));
        assert!(!dotted.is_match("nested/deep/plugin.lua"));
    }

    #[test]
    fn invalid_syntax_has_a_location() {
        let error = Pattern::compile("[abc", PatternOptions::default()).unwrap_err();
        assert_eq!(error.offset(), 0);
        assert_eq!(error.message(), "unclosed character class");
        assert!(compile("foo\\").is_match("foo\\"));
    }

    #[test]
    fn has_wildcards_respects_brace_and_extglob_options() {
        let plain = PatternOptions::default();
        assert!(!Pattern::has_wildcards("literal", plain));
        assert!(Pattern::has_wildcards("file?.rs", plain));
        assert!(Pattern::has_wildcards("[[:alpha:]]", plain));
        assert!(!Pattern::has_wildcards("{src,lib}", plain));
        assert!(Pattern::has_wildcards(
            "{src,lib}",
            PatternOptions::default().braces(true)
        ));
        assert!(!Pattern::has_wildcards("@(src|lib)", plain));
        assert!(Pattern::has_wildcards(
            "@(src|lib)",
            PatternOptions::default().extglob(true)
        ));
        assert!(!Pattern::has_wildcards(
            "literal(",
            PatternOptions::default().extglob(true)
        ));
    }

    fn byte_words(alphabet: &[u8], max_length: usize) -> Vec<Vec<u8>> {
        let mut words = vec![Vec::new()];
        let mut current = vec![Vec::new()];
        for _ in 0..max_length {
            let mut next = Vec::new();
            for prefix in current {
                for &byte in alphabet {
                    let mut word = prefix.clone();
                    word.push(byte);
                    next.push(word);
                }
            }
            words.extend(next.iter().cloned());
            current = next;
        }
        words
    }
}
