#![forbid(unsafe_code)]
#![doc = "Compile-checked Markdown documentation for Ferralk."]

#[cfg(test)]
const DOCUMENTS: &[&str] = &[
    "CHANGELOG.md",
    "CONTRIBUTING.md",
    "README.md",
    "RFC-zig-free-zlob-port.md",
    "crates/ferralk-glob/README.md",
    "crates/ferralk/README.md",
    "docs/README.md",
    "docs/benchmark-evidence.md",
    "docs/usage.md",
    "docs/compatibility-guide.md",
    "docs/compatibility-matrix.md",
    "docs/corpus-format.md",
    "docs/external-release-gates.md",
    "docs/fast-glob-reference.md",
    "docs/palamedes-adoption.md",
    "docs/zlob-1.6.3-reference.md",
    "docs/zlob-fnmatch-test-coverage.md",
    "docs/zlob-test-suite-audit.md",
    "docs/adr/README.md",
    "docs/adr/0001-independent-port-under-the-ferralk-name.md",
    "docs/adr/0002-hybrid-port-strategy.md",
    "docs/adr/0003-two-published-crates-no-c-abi.md",
    "docs/adr/0004-msrv-stable-minus-two.md",
    "docs/adr/0005-byte-matching-wtf8-on-windows.md",
    "docs/adr/0006-git-normative-ignore-semantics.md",
    "docs/adr/0007-differential-corpus-and-dev-time-oracle.md",
    "docs/adr/0008-simd-via-memchr-primitives.md",
    "docs/adr/0009-own-work-stealing-scheduler.md",
    "docs/adr/0010-portable-1.0-native-backends-macos-then-linux.md",
    "docs/adr/0011-posix-conservative-walker-defaults.md",
    "docs/adr/0012-ferroni-repository-blueprint.md",
    "docs/adr/0013-no-glob-to-regex-translation.md",
    "docs/adr/0014-own-gitignore-rule-matching.md",
    "fuzz/README.md",
];

#[cfg(test)]
const EXPECTED_COMPILED_RUST_FENCES: usize = 17;

#[cfg(test)]
struct FencePolicy {
    path: &'static str,
    compiled_rust_fences: usize,
    ignored_rust_fences: usize,
    intentional_text_fragments: &'static [&'static str],
}

#[cfg(test)]
const FENCE_POLICIES: &[FencePolicy] = &[
    FencePolicy {
        path: "CHANGELOG.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "CONTRIBUTING.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "README.md",
        compiled_rust_fences: 2,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "RFC-zig-free-zlob-port.md",
        compiled_rust_fences: 3,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "crates/ferralk-glob/README.md",
        compiled_rust_fences: 1,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "crates/ferralk/README.md",
        compiled_rust_fences: 1,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/README.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/benchmark-evidence.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/usage.md",
        compiled_rust_fences: 5,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/compatibility-guide.md",
        compiled_rust_fences: 5,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/compatibility-matrix.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/corpus-format.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/external-release-gates.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/fast-glob-reference.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/palamedes-adoption.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &["Walker::new(first)"],
    },
    FencePolicy {
        path: "docs/zlob-1.6.3-reference.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/zlob-fnmatch-test-coverage.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/zlob-test-suite-audit.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/README.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0001-independent-port-under-the-ferralk-name.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0002-hybrid-port-strategy.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0003-two-published-crates-no-c-abi.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0004-msrv-stable-minus-two.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0005-byte-matching-wtf8-on-windows.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0006-git-normative-ignore-semantics.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0007-differential-corpus-and-dev-time-oracle.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0008-simd-via-memchr-primitives.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0009-own-work-stealing-scheduler.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0010-portable-1.0-native-backends-macos-then-linux.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0011-posix-conservative-walker-defaults.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0012-ferroni-repository-blueprint.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0013-no-glob-to-regex-translation.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0014-own-gitignore-rule-matching.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "fuzz/README.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
];

#[doc = include_str!("../../../CHANGELOG.md")]
pub mod changelog {}

#[doc = include_str!("../../../CONTRIBUTING.md")]
pub mod contributing {}

#[doc = include_str!("../../../README.md")]
pub mod repository_readme {}

#[doc = include_str!("../../../RFC-zig-free-zlob-port.md")]
pub mod rfc {}

#[doc = include_str!("../../../crates/ferralk-glob/README.md")]
pub mod ferralk_glob_readme {}

#[doc = include_str!("../../../crates/ferralk/README.md")]
pub mod ferralk_readme {}

#[doc = include_str!("../../../docs/README.md")]
pub mod index {}

#[doc = include_str!("../../../docs/benchmark-evidence.md")]
pub mod benchmark_evidence {}

#[doc = include_str!("../../../docs/usage.md")]
pub mod usage {}

#[doc = include_str!("../../../docs/compatibility-guide.md")]
pub mod compatibility_guide {}

#[doc = include_str!("../../../docs/compatibility-matrix.md")]
pub mod compatibility_matrix {}

#[doc = include_str!("../../../docs/corpus-format.md")]
pub mod corpus_format {}

#[doc = include_str!("../../../docs/external-release-gates.md")]
pub mod external_release_gates {}

#[doc = include_str!("../../../docs/fast-glob-reference.md")]
pub mod fast_glob_reference {}

#[doc = include_str!("../../../docs/palamedes-adoption.md")]
pub mod palamedes_adoption {}

#[doc = include_str!("../../../docs/zlob-1.6.3-reference.md")]
pub mod zlob_1_6_3_reference {}

#[doc = include_str!("../../../docs/zlob-fnmatch-test-coverage.md")]
pub mod zlob_fnmatch_test_coverage {}

#[doc = include_str!("../../../docs/zlob-test-suite-audit.md")]
pub mod zlob_test_suite_audit {}

#[doc = include_str!("../../../docs/adr/README.md")]
pub mod adr_index {}

#[doc = include_str!("../../../docs/adr/0001-independent-port-under-the-ferralk-name.md")]
pub mod adr_0001 {}

#[doc = include_str!("../../../docs/adr/0002-hybrid-port-strategy.md")]
pub mod adr_0002 {}

#[doc = include_str!("../../../docs/adr/0003-two-published-crates-no-c-abi.md")]
pub mod adr_0003 {}

#[doc = include_str!("../../../docs/adr/0004-msrv-stable-minus-two.md")]
pub mod adr_0004 {}

#[doc = include_str!("../../../docs/adr/0005-byte-matching-wtf8-on-windows.md")]
pub mod adr_0005 {}

#[doc = include_str!("../../../docs/adr/0006-git-normative-ignore-semantics.md")]
pub mod adr_0006 {}

#[doc = include_str!("../../../docs/adr/0007-differential-corpus-and-dev-time-oracle.md")]
pub mod adr_0007 {}

#[doc = include_str!("../../../docs/adr/0008-simd-via-memchr-primitives.md")]
pub mod adr_0008 {}

#[doc = include_str!("../../../docs/adr/0009-own-work-stealing-scheduler.md")]
pub mod adr_0009 {}

#[doc = include_str!("../../../docs/adr/0010-portable-1.0-native-backends-macos-then-linux.md")]
pub mod adr_0010 {}

#[doc = include_str!("../../../docs/adr/0011-posix-conservative-walker-defaults.md")]
pub mod adr_0011 {}

#[doc = include_str!("../../../docs/adr/0012-ferroni-repository-blueprint.md")]
pub mod adr_0012 {}

#[doc = include_str!("../../../docs/adr/0013-no-glob-to-regex-translation.md")]
pub mod adr_0013 {}

#[doc = include_str!("../../../docs/adr/0014-own-gitignore-rule-matching.md")]
pub mod adr_0014 {}

#[doc = include_str!("../../../fuzz/README.md")]
pub mod fuzzing {}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

    use serde_json::Value;

    use super::{DOCUMENTS, EXPECTED_COMPILED_RUST_FENCES, FENCE_POLICIES, FencePolicy};

    #[derive(Debug)]
    struct MarkdownFence {
        info: String,
        first_content_line: Option<String>,
    }

    #[test]
    fn every_markdown_document_is_included() {
        let repository_root = repository_root();
        let mut discovered = BTreeSet::new();
        collect_markdown_files(&repository_root, &repository_root, &mut discovered);

        let included = DOCUMENTS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            included, discovered,
            "add each new Markdown document to this doctest harness"
        );
    }

    #[test]
    fn every_document_has_the_expected_doctest_fence_policy() {
        let documented_paths = DOCUMENTS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<BTreeSet<_>>();
        let policy_paths = FENCE_POLICIES
            .iter()
            .map(|policy| policy.path.to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            policy_paths, documented_paths,
            "each harness document needs an explicit Rust-fence policy"
        );

        let repository_root = repository_root();
        let actual_compiled_fences = FENCE_POLICIES
            .iter()
            .map(|policy| {
                let markdown = fs::read_to_string(repository_root.join(policy.path))
                    .expect("inventory Markdown document is readable");
                let fences = scan_fenced_code_blocks(&markdown);
                assert_fence_policy(policy, &fences);
                fences
                    .iter()
                    .filter(|fence| {
                        is_rust_fence(&fence.info) && !is_ignored_rust_fence(&fence.info)
                    })
                    .count()
            })
            .sum::<usize>();

        assert_eq!(
            actual_compiled_fences, EXPECTED_COMPILED_RUST_FENCES,
            "the doctest harness must retain the deliberately compiled Rust-fence total"
        );
    }

    #[test]
    fn release_please_versioned_consumer_docs_match_the_workspace() {
        let repository_root = repository_root();
        let workspace_version = workspace_version(&repository_root);
        let release_please =
            fs::read_to_string(repository_root.join(".release-please-config.json"))
                .expect("Release Please configuration is readable");
        let release_please: Value =
            serde_json::from_str(&release_please).expect("Release Please configuration is JSON");

        let extra_files = release_please["packages"]["."]["extra-files"]
            .as_array()
            .expect("Release Please package has extra files");
        let expected_documents = [
            ("README.md", 3_usize),
            ("docs/usage.md", 1_usize),
            ("docs/external-release-gates.md", 1_usize),
        ];

        for (path, expected_annotation_count) in expected_documents {
            assert!(
                extra_files
                    .iter()
                    .any(|entry| { entry["type"] == "generic" && entry["path"] == path }),
                "Release Please must update every versioned consumer document: {path}"
            );

            let document = fs::read_to_string(repository_root.join(path))
                .unwrap_or_else(|error| panic!("{path} is readable: {error}"));
            let annotated_lines = document
                .lines()
                .filter(|line| line.contains("x-release-please-version"))
                .collect::<Vec<_>>();
            assert_eq!(
                annotated_lines.len(),
                expected_annotation_count,
                "every consumer-facing current-version reference in {path} must be annotated"
            );

            for line in annotated_lines {
                let versions = semver_values(line);
                assert_eq!(
                    versions,
                    vec![workspace_version.as_str()],
                    "the Release Please annotation in {path} must select exactly the workspace version: {line}"
                );
            }
        }
    }

    #[test]
    fn fence_scanner_matches_rustdoc_fence_classification_and_real_closers() {
        let fences = scan_fenced_code_blocks(
            "```\nlet bare: u8 = 1;\n```\n\
             ```no_run,rust\nlet explicit: u8 = 2;\n```\n\
             ```should_panic\npanic!(\"expected\");\n```\n\
             ```edition2024,compile_fail\nlet _: u8 = \"not a byte\";\n```\n\
             ```ignore-x86_64\nlet skipped = true;\n```\n\
             ```text\nWalker::new(first)\n```\n\
             ```sh\necho not-rust\n```\n\
             ```json\n{\"not\": \"rust\"}\n```\n\
             ~~~rust\nlet tilde_fence = true;\n~~~~\n\
             ````rust\n// ```text is content, not a nested fence\n````\n",
        );

        assert_eq!(
            fences
                .iter()
                .filter(|fence| is_rust_fence(&fence.info) && !is_ignored_rust_fence(&fence.info))
                .count(),
            6
        );
        assert_eq!(
            fences
                .iter()
                .filter(|fence| is_rust_fence(&fence.info) && is_ignored_rust_fence(&fence.info))
                .count(),
            1
        );
        assert_eq!(
            fences[9].first_content_line.as_deref(),
            Some("// ```text is content, not a nested fence")
        );
        assert!(is_rust_fence("no_run,sh"));
        assert!(is_rust_fence("no_run,rust"));
        assert!(is_rust_fence("rust,no_run"));
        assert!(!is_rust_fence("sh,no_run"));
        assert!(!is_rust_fence("json,should_panic"));
        assert!(!is_rust_fence("unknown-language,should_panic"));
        assert!(is_rust_fence("should_panic,unknown-language"));
        assert!(is_rust_fence("sh,rust"));
        assert!(is_rust_fence("rust,sh"));
        assert!(is_rust_fence("ignore-x86_64,no_run"));
        assert!(is_ignored_rust_fence("ignore-x86_64,no_run"));
        assert!(is_rust_fence("edition2024,compile_fail"));
        assert!(is_rust_fence("compile_fail,edition2024"));
        assert!(is_rust_fence("  no_run,  rust  "));
        assert!(is_rust_fence("compile_fail,E0308"));
        assert!(is_rust_fence("test_harness"));
        assert!(is_rust_fence("standalone_crate"));
        assert!(!is_rust_fence("allow_fail"));
        assert!(!is_rust_fence("unknown-language"));
    }

    fn repository_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn workspace_version(repository_root: &Path) -> String {
        fs::read_to_string(repository_root.join("Cargo.toml"))
            .expect("workspace manifest is readable")
            .lines()
            .skip_while(|line| line.trim() != "[workspace.package]")
            .skip(1)
            .find_map(|line| {
                line.trim()
                    .strip_prefix("version = \"")
                    .and_then(|value| value.strip_suffix('\"'))
            })
            .map(str::to_owned)
            .expect("workspace package declares a version")
    }

    fn semver_values(line: &str) -> Vec<&str> {
        let bytes = line.as_bytes();
        let mut versions = Vec::new();
        let mut index = 0;

        while index < bytes.len() {
            if !bytes[index].is_ascii_digit() {
                index += 1;
                continue;
            }

            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index == bytes.len() || bytes[index] != b'.' {
                continue;
            }
            index += 1;
            let minor_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if minor_start == index || index == bytes.len() || bytes[index] != b'.' {
                continue;
            }
            index += 1;
            let patch_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if patch_start != index {
                versions.push(&line[start..index]);
            }
        }

        versions
    }

    fn assert_fence_policy(policy: &FencePolicy, fences: &[MarkdownFence]) {
        let compiled_rust_fences = fences
            .iter()
            .filter(|fence| is_rust_fence(&fence.info) && !is_ignored_rust_fence(&fence.info))
            .count();
        let ignored_rust_fences = fences
            .iter()
            .filter(|fence| is_rust_fence(&fence.info) && is_ignored_rust_fence(&fence.info))
            .count();
        let text_fences = fences
            .iter()
            .filter(|fence| fence_language(&fence.info) == Some("text"))
            .collect::<Vec<_>>();

        assert_eq!(
            compiled_rust_fences, policy.compiled_rust_fences,
            "Rust-fence policy changed for {}: update this inventory only when the changed fence is intentionally compiled by rustdoc",
            policy.path
        );
        assert_eq!(
            ignored_rust_fences, policy.ignored_rust_fences,
            "ignored Rust-fence policy changed for {}: `rust,ignore` needs an explicit, narrow justification in this inventory",
            policy.path
        );
        assert_eq!(
            text_fences.len(),
            policy.intentional_text_fragments.len(),
            "text-fence policy changed for {}: intentionally non-Rust fragments must be explicitly inventoried",
            policy.path
        );

        for expected_first_line in policy.intentional_text_fragments {
            let matching_fragments = text_fences
                .iter()
                .filter(|fence| fence.first_content_line.as_deref() == Some(*expected_first_line))
                .count();
            assert_eq!(
                matching_fragments, 1,
                "intentional text fragment changed for {}: expected first content line `{expected_first_line}`",
                policy.path
            );
        }
    }

    fn scan_fenced_code_blocks(markdown: &str) -> Vec<MarkdownFence> {
        let mut fences: Vec<MarkdownFence> = Vec::new();
        let mut open_fence = None;

        for line in markdown.lines() {
            if let Some((marker, marker_len)) = open_fence {
                if is_closing_fence(line, marker, marker_len) {
                    open_fence = None;
                } else if let Some(fence) = fences.last_mut()
                    && fence.first_content_line.is_none()
                    && !line.trim().is_empty()
                {
                    fence.first_content_line = Some(line.trim().to_owned());
                }
                continue;
            }

            if let Some((marker, marker_len, info)) = opening_fence(line) {
                fences.push(MarkdownFence {
                    info: info.to_owned(),
                    first_content_line: None,
                });
                open_fence = Some((marker, marker_len));
            }
        }

        fences
    }

    fn opening_fence(line: &str) -> Option<(u8, usize, &str)> {
        let (indent, rest) = leading_spaces(line);
        if indent > 3 {
            return None;
        }

        let marker = *rest.as_bytes().first()?;
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let marker_len = rest.bytes().take_while(|byte| *byte == marker).count();
        if marker_len < 3 {
            return None;
        }

        let info = rest[marker_len..].trim();
        if marker == b'`' && info.contains('`') {
            return None;
        }
        Some((marker, marker_len, info))
    }

    fn is_closing_fence(line: &str, marker: u8, marker_len: usize) -> bool {
        let (indent, rest) = leading_spaces(line);
        if indent > 3 {
            return false;
        }
        let closing_len = rest.bytes().take_while(|byte| *byte == marker).count();
        closing_len >= marker_len && rest[closing_len..].trim().is_empty()
    }

    fn leading_spaces(line: &str) -> (usize, &str) {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        (indent, &line[indent..])
    }

    fn is_rust_fence(info: &str) -> bool {
        let tokens = fence_tokens(info).collect::<Vec<_>>();
        if tokens.is_empty() {
            return true;
        }
        if tokens.contains(&"rust") {
            return true;
        }
        is_rustdoc_rust_attribute(tokens[0])
    }

    fn is_ignored_rust_fence(info: &str) -> bool {
        fence_tokens(info).any(is_ignore_attribute)
    }

    fn fence_language(info: &str) -> Option<&str> {
        fence_tokens(info).next()
    }

    fn fence_tokens(info: &str) -> impl Iterator<Item = &str> {
        info.split(|character: char| character == ',' || character.is_ascii_whitespace())
            .filter(|part| !part.is_empty())
    }

    fn is_rustdoc_rust_attribute(token: &str) -> bool {
        matches!(
            token,
            "no_run"
                | "should_panic"
                | "compile_fail"
                | "test_harness"
                | "standalone_crate"
                | "edition2015"
                | "edition2018"
                | "edition2021"
                | "edition2024"
        ) || is_ignore_attribute(token)
            || is_error_code(token)
    }

    fn is_ignore_attribute(token: &str) -> bool {
        token == "ignore" || token.starts_with("ignore-")
    }

    fn is_error_code(token: &str) -> bool {
        let bytes = token.as_bytes();
        bytes.len() == 5 && bytes[0] == b'E' && bytes[1..].iter().all(|byte| byte.is_ascii_digit())
    }

    fn collect_markdown_files(root: &Path, directory: &Path, discovered: &mut BTreeSet<String>) {
        for entry in fs::read_dir(directory).expect("repository directory is readable") {
            let entry = entry.expect("repository directory entry is readable");
            let path = entry.path();
            if path.is_dir() {
                if !is_excluded_directory(root, &path) {
                    collect_markdown_files(root, &path, discovered);
                }
            } else if path.extension().is_some_and(|extension| extension == "md") {
                discovered.insert(
                    path.strip_prefix(root)
                        .expect("documentation file is below the docs root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }

    fn is_excluded_directory(root: &Path, directory: &Path) -> bool {
        // Git metadata and generated/build trees are not repository
        // documentation. Every other Markdown file is inventory-controlled.
        directory
            .strip_prefix(root)
            .expect("repository directory is below the repository root")
            .components()
            .any(|component| {
                matches!(
                    component.as_os_str().to_str(),
                    Some(".git" | "target" | "vendor")
                )
            })
    }
}
