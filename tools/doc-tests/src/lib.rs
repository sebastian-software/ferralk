#![forbid(unsafe_code)]
#![doc = "Compile-checked Markdown documentation for Ferralk."]

#[cfg(test)]
const DOCUMENTS: &[&str] = &[
    ".github/pull_request_template.md",
    "AGENTS.md",
    "CHANGELOG.md",
    "CLAUDE.md",
    "CODE_OF_CONDUCT.md",
    "CONTRIBUTING.md",
    "README.md",
    "RFC-zig-free-zlob-port.md",
    "SECURITY.md",
    "SUPPORT.md",
    "crates/ferralk-glob/README.md",
    "crates/ferralk/README.md",
    "docs/README.md",
    "docs/benchmark-evidence.md",
    "docs/usage.md",
    "docs/verification-comparison.md",
    "docs/compatibility-guide.md",
    "docs/compatibility-matrix.md",
    "docs/consumer-contract.md",
    "docs/corpus-format.md",
    "docs/external-release-gates.md",
    "docs/fast-glob-reference.md",
    "docs/palamedes-adoption.md",
    "docs/stability.md",
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
    "docs/adr/0015-posix-escapes-in-bracket-classes.md",
    "docs/adr/0016-shell-star-runs-before-extglobs.md",
    "docs/adr/0017-caller-owned-list-api-conventions.md",
    "docs/adr/0018-dual-mit-apache-license.md",
    "docs/adr/0019-dependency-version-requirements.md",
    "docs/adr/0020-double-star-only-as-a-whole-component.md",
    "docs/adr/0021-dependency-range-policy.md",
    "docs/readme-theme.md",
    "docs/adr/readme-theme-composition.md",
    "fuzz/README.md",
    "skills/ferralk/SKILL.md",
    "homepage/README.md",
    "homepage/content/getting-started.md",
];

#[cfg(test)]
const EXPECTED_COMPILED_RUST_FENCES: usize = 27;

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
        path: "homepage/README.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "homepage/content/getting-started.md",
        compiled_rust_fences: 2,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: ".github/pull_request_template.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "AGENTS.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "CHANGELOG.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "CLAUDE.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "CODE_OF_CONDUCT.md",
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
        path: "SECURITY.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "SUPPORT.md",
        compiled_rust_fences: 0,
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
        compiled_rust_fences: 6,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/verification-comparison.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/compatibility-guide.md",
        compiled_rust_fences: 6,
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
        path: "docs/consumer-contract.md",
        compiled_rust_fences: 3,
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
        path: "docs/stability.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
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
        path: "docs/adr/0015-posix-escapes-in-bracket-classes.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0016-shell-star-runs-before-extglobs.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0017-caller-owned-list-api-conventions.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0018-dual-mit-apache-license.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0019-dependency-version-requirements.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0020-double-star-only-as-a-whole-component.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/0021-dependency-range-policy.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/readme-theme.md",
        compiled_rust_fences: 0,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
    FencePolicy {
        path: "docs/adr/readme-theme-composition.md",
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
    FencePolicy {
        path: "skills/ferralk/SKILL.md",
        compiled_rust_fences: 3,
        ignored_rust_fences: 0,
        intentional_text_fragments: &[],
    },
];

#[doc = include_str!("../../../.github/pull_request_template.md")]
pub mod pull_request_template {}

#[doc = include_str!("../../../AGENTS.md")]
pub mod agents {}

#[doc = include_str!("../../../CHANGELOG.md")]
pub mod changelog {}

#[doc = include_str!("../../../CLAUDE.md")]
pub mod claude {}

#[doc = include_str!("../../../CODE_OF_CONDUCT.md")]
pub mod code_of_conduct {}

#[doc = include_str!("../../../CONTRIBUTING.md")]
pub mod contributing {}

#[doc = include_str!("../../../README.md")]
pub mod repository_readme {}

#[doc = include_str!("../../../RFC-zig-free-zlob-port.md")]
pub mod rfc {}

#[doc = include_str!("../../../SECURITY.md")]
pub mod security {}

#[doc = include_str!("../../../SUPPORT.md")]
pub mod support {}

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

#[doc = include_str!("../../../docs/verification-comparison.md")]
pub mod verification_comparison {}

#[doc = include_str!("../../../docs/compatibility-guide.md")]
pub mod compatibility_guide {}

#[doc = include_str!("../../../docs/compatibility-matrix.md")]
pub mod compatibility_matrix {}

#[doc = include_str!("../../../docs/consumer-contract.md")]
pub mod consumer_contract {}

#[doc = include_str!("../../../docs/corpus-format.md")]
pub mod corpus_format {}

#[doc = include_str!("../../../docs/external-release-gates.md")]
pub mod external_release_gates {}

#[doc = include_str!("../../../docs/fast-glob-reference.md")]
pub mod fast_glob_reference {}

#[doc = include_str!("../../../docs/palamedes-adoption.md")]
pub mod palamedes_adoption {}

#[doc = include_str!("../../../docs/stability.md")]
pub mod stability {}

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

#[doc = include_str!("../../../docs/adr/0015-posix-escapes-in-bracket-classes.md")]
pub mod adr_0015 {}

#[doc = include_str!("../../../docs/adr/0016-shell-star-runs-before-extglobs.md")]
pub mod adr_0016 {}

#[doc = include_str!("../../../docs/adr/0017-caller-owned-list-api-conventions.md")]
pub mod adr_0017 {}

#[doc = include_str!("../../../docs/adr/0018-dual-mit-apache-license.md")]
pub mod adr_0018 {}

#[doc = include_str!("../../../docs/adr/0019-dependency-version-requirements.md")]
pub mod adr_0019 {}

#[doc = include_str!("../../../docs/adr/0020-double-star-only-as-a-whole-component.md")]
pub mod adr_0020 {}

#[doc = include_str!("../../../docs/adr/0021-dependency-range-policy.md")]
pub mod adr_0021 {}

#[doc = include_str!("../../../docs/readme-theme.md")]
pub mod readme_theme {}

#[doc = include_str!("../../../docs/adr/readme-theme-composition.md")]
pub mod readme_theme_composition {}

#[doc = include_str!("../../../fuzz/README.md")]
pub mod fuzzing {}

#[doc = include_str!("../../../skills/ferralk/SKILL.md")]
pub mod consumer_skill {}

#[doc = include_str!("../../../homepage/README.md")]
pub mod website_readme {}

#[doc = include_str!("../../../homepage/content/getting-started.md")]
pub mod website_getting_started {}

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

    /// Cargo packages a crate's own directory, so `ferralk-glob` carries a
    /// copy of the repository's license texts and notice. ADR-0018 keeps the
    /// same copies next to the `ferralk` sources in `crates/ferralk/`. The
    /// copies are kept identical to the root files rather than maintained
    /// twice. Both halves of the ADR-0018 dual license have to travel with
    /// each crate.
    #[test]
    fn published_crates_ship_the_repository_license_and_notice() {
        let repository_root = repository_root();
        for crate_directory in ["crates/ferralk-glob", "crates/ferralk"] {
            for file in ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE"] {
                let expected =
                    fs::read(repository_root.join(file)).expect("repository file is readable");
                let actual = fs::read(repository_root.join(crate_directory).join(file))
                    .unwrap_or_else(|error| {
                        panic!("{crate_directory}/{file} is packaged: {error}")
                    });
                assert!(
                    actual == expected,
                    "{crate_directory}/{file} must be a byte-for-byte copy of the repository {file}"
                );
            }
        }
    }

    /// `ferralk` is the root package, so its tarball is cut from the
    /// repository root and takes the root license texts and notice directly.
    /// Its `include` list is what puts them there. The patterns are anchored
    /// with a leading `/`: `include` uses gitignore syntax, where a bare
    /// `LICENSE-MIT` also matches the ADR-0018 copies under `crates/`.
    #[test]
    fn the_root_package_packages_the_repository_license_and_notice() {
        let manifest = fs::read_to_string(repository_root().join("Cargo.toml"))
            .expect("root manifest is readable");
        let include = manifest
            .lines()
            .skip_while(|line| line.trim() != "include = [")
            .skip(1)
            .take_while(|line| line.trim() != "]")
            .map(|line| line.trim().trim_end_matches(','))
            .collect::<Vec<_>>();
        for file in ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE"] {
            assert!(
                include.contains(&format!("\"/{file}\"").as_str()),
                "the root ferralk package must include the repository {file}: {include:?}"
            );
        }
    }

    /// The project-owned Release Please contract: one component at `.`, the
    /// `rust` strategy, and no Cargo manifest in
    /// `extra-files`. The strategy versions the root package, every listed
    /// workspace member, their internal requirements, and the root
    /// `Cargo.lock` itself; it can do that only for members listed by explicit
    /// path and carrying a concrete `version`.
    #[test]
    fn release_please_versions_the_workspace_through_the_rust_strategy() {
        let repository_root = repository_root();
        let release_please = release_please_config(&repository_root);
        let packages = release_please["packages"]
            .as_object()
            .expect("Release Please configuration lists packages");
        assert_eq!(
            packages.keys().collect::<Vec<_>>(),
            vec!["."],
            "Release Please must release one product component at the repository root"
        );
        assert_eq!(release_please["release-type"], "rust");
        assert_eq!(packages["."]["component"], "ferralk");
        assert!(
            packages["."].get("version-file").is_none()
                && packages["."].get("release-type").is_none(),
            "the root component must take the top-level rust strategy, not a version file"
        );

        // The fuzz workspace keeps its own lockfile, which the strategy does
        // not see, so those three lockfile entries are the only Cargo files
        // Release Please is pointed at directly.
        let cargo_extra_files = packages["."]["extra-files"]
            .as_array()
            .expect("Release Please package has extra files")
            .iter()
            .filter(|entry| entry["type"] != "generic")
            .map(|entry| format!("{} {}", entry["path"], entry["jsonpath"]))
            .collect::<Vec<_>>();
        let expected_cargo_extra_files = ["ferralk", "ferralk-glob", "corpus"]
            .map(|name| {
                format!(
                    "\"fuzz/Cargo.lock\" \"$.package[?(@.name.value==\\\"{name}\\\")].version\""
                )
            })
            .to_vec();
        assert_eq!(
            cargo_extra_files, expected_cargo_extra_files,
            "only the separate fuzz workspace's lockfile may be a typed extra file"
        );

        let product_version = product_version(&repository_root);
        let root_manifest = fs::read_to_string(repository_root.join("Cargo.toml"))
            .expect("root manifest is readable");
        let members = root_manifest
            .lines()
            .skip_while(|line| line.trim() != "members = [")
            .skip(1)
            .take_while(|line| line.trim() != "]")
            .map(|line| line.trim().trim_end_matches(',').trim_matches('"'))
            .collect::<Vec<_>>();
        assert!(
            members.contains(&"crates/ferralk-glob"),
            "ferralk-glob must stay a workspace member: {members:?}"
        );
        for member in members {
            assert!(
                !member.contains('*'),
                "Release Please does not expand member globs: {member}"
            );
            let manifest = fs::read_to_string(repository_root.join(member).join("Cargo.toml"))
                .unwrap_or_else(|error| panic!("{member}/Cargo.toml is readable: {error}"));
            assert_eq!(
                package_version(&manifest),
                Some(product_version.as_str()),
                "{member} must carry the product version as a concrete `version`"
            );
        }

        assert!(
            root_manifest.lines().any(|line| line
                == format!(
                    "ferralk-glob = {{ path = \"crates/ferralk-glob\", version = \"{product_version}\" }}"
                )),
            "ferralk must require ferralk-glob at the product version by both path and version"
        );

        let fuzz_lock = fs::read_to_string(repository_root.join("fuzz/Cargo.lock"))
            .expect("fuzz lockfile is readable");
        for name in ["ferralk", "ferralk-glob", "corpus"] {
            let entry = format!("name = \"{name}\"");
            let version = fuzz_lock
                .lines()
                .skip_while(|line| *line != entry)
                .nth(1)
                .unwrap_or_else(|| panic!("fuzz/Cargo.lock locks {name}"));
            assert_eq!(
                version,
                format!("version = \"{product_version}\""),
                "fuzz/Cargo.lock must lock {name} at the product version"
            );
        }
    }

    #[test]
    fn release_please_versioned_consumer_docs_match_the_workspace() {
        let repository_root = repository_root();
        let workspace_version = product_version(&repository_root);
        let release_please = release_please_config(&repository_root);

        let extra_files = release_please["packages"]["."]["extra-files"]
            .as_array()
            .expect("Release Please package has extra files");
        let expected_documents = [
            ("README.md", 3_usize),
            ("README.md.src", 3_usize),
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

    /// The user-space CPU gate is declared once, as `CPU_GATE_PERCENT` in the
    /// walker benchmark workflow, and the acceptance marker once, in the tool
    /// that applies the gate. The benchmark evidence and CONTRIBUTING restate
    /// both, so this contract holds them to their sources. Line by line, like
    /// the coverage contract above.
    #[test]
    fn the_cpu_gate_is_declared_once_and_restated_consistently() {
        let repository_root = repository_root();
        let read = |path: &str| {
            fs::read_to_string(repository_root.join(path))
                .unwrap_or_else(|error| panic!("{path} is readable: {error}"))
        };
        let workflow = read(".github/workflows/walker-bench.yml");
        let declarations = workflow
            .lines()
            .filter_map(|line| line.trim().strip_prefix("CPU_GATE_PERCENT:"))
            .map(|value| value.trim().trim_matches('"'))
            .collect::<Vec<_>>();
        assert_eq!(
            declarations.len(),
            1,
            "the CPU gate must be declared exactly once, as CPU_GATE_PERCENT"
        );
        let percent = declarations[0];
        assert!(
            workflow
                .lines()
                .any(|line| line.contains("--gate-percent \"$CPU_GATE_PERCENT\"")),
            "the CPU job must gate on CPU_GATE_PERCENT, not on a literal threshold"
        );

        let tool = read("tools/bench/src/bin/compare_callgrind.rs");
        let marker = tool
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("const ACCEPTANCE_MARKER: &str = \"")?
                    .strip_suffix("\";")
            })
            .expect("compare_callgrind declares ACCEPTANCE_MARKER");

        let evidence = read("docs/benchmark-evidence.md");
        let lane_row = evidence
            .lines()
            .find(|line| line.starts_with("| User-space CPU |"))
            .expect("the lanes table lists the user-space CPU lane");
        let restated = format!("more than {percent}% over its merge base");
        assert!(
            lane_row.contains(&restated),
            "the lanes table must state the {percent}% CPU gate: {lane_row}"
        );
        assert!(
            evidence
                .lines()
                .any(|line| line.contains(&format!("by more than **{percent}%**"))),
            "the CPU gate section must state the {percent}% threshold"
        );

        let contributing = read("CONTRIBUTING.md");
        assert!(
            contributing.lines().any(|line| line.contains(&restated)),
            "CONTRIBUTING must state the {percent}% CPU gate"
        );
        for path in ["README.md.src", "README.md"] {
            let readme = normalize_whitespace(&read(path));
            assert!(
                readme.contains(&restated),
                "{path} must state the {percent}% CPU gate"
            );
        }
        for (path, text) in [
            ("CONTRIBUTING.md", &contributing),
            ("docs/benchmark-evidence.md", &evidence),
        ] {
            assert!(
                text.lines()
                    .any(|line| line.contains(&format!("`{marker}`"))),
                "{path} must name the acceptance marker {marker:?}"
            );
        }
    }

    /// The minimum supported Rust version is declared once under
    /// `[workspace.package]` in the root `Cargo.toml`. Both `README.md.src`
    /// and the generated `README.md` carry a badge restating it, so this
    /// contract holds both copies to the workspace manifest.
    #[test]
    fn the_msrv_badge_matches_workspace_rust_version() {
        let repository_root = repository_root();
        let msrv = workspace_rust_version(&repository_root);

        for path in ["README.md.src", "README.md"] {
            let document = fs::read_to_string(repository_root.join(path))
                .unwrap_or_else(|error| panic!("{path} is readable: {error}"));
            let badge = document
                .lines()
                .find(|line| line.contains("img.shields.io/badge/MSRV-"))
                .unwrap_or_else(|| panic!("{path} carries the MSRV badge"));
            assert!(
                badge.contains(&format!("[![MSRV {msrv}]")),
                "the {path} MSRV badge must read {msrv} in its link text: {badge}"
            );
            assert!(
                badge.contains(&format!("img.shields.io/badge/MSRV-{msrv}-")),
                "the {path} MSRV badge must render {msrv} in its image: {badge}"
            );
        }
    }

    /// The corpus total and divergence count restated in `README.md.src` (and
    /// generated `README.md`) must match the checked-in `corpus/*.jsonl` files.
    #[test]
    fn readme_corpus_counts_match_the_checked_in_corpus() {
        let repository_root = repository_root();
        let (total_cases, divergences) = corpus_case_and_divergence_counts(&repository_root);
        let expected_cases = format!(
            "The {} checked-in behavioral cases",
            format_with_commas(total_cases)
        );
        let expected_divergences =
            format!("and all {} of them do.", format_with_commas(divergences));

        for path in ["README.md.src", "README.md"] {
            let document = fs::read_to_string(repository_root.join(path))
                .unwrap_or_else(|error| panic!("{path} is readable: {error}"));
            let normalized = normalize_whitespace(&document);
            assert!(
                normalized.contains(&expected_cases),
                "{path} must state the exact corpus case count ({expected_cases:?})"
            );
            assert!(
                normalized.contains(&expected_divergences),
                "{path} must state the exact corpus divergence count ({expected_divergences:?})"
            );
        }
    }

    /// `docs/corpus-format.md` enumerates every topic file under `corpus/`.
    /// Keep that prose inventory exact so new or renamed `.jsonl` files never
    /// disappear from the format guide.
    #[test]
    fn corpus_format_topic_list_matches_corpus_directory() {
        let repository_root = repository_root();
        let discovered = corpus_topic_files(&repository_root);
        assert!(
            !discovered.is_empty(),
            "corpus/ must contain at least one .jsonl topic file"
        );

        let guide = fs::read_to_string(repository_root.join("docs/corpus-format.md"))
            .expect("docs/corpus-format.md is readable");
        let normalized = normalize_whitespace(&guide);
        let after_prefix = normalized
            .split_once("The topic files are ")
            .expect("docs/corpus-format.md lists the corpus topic files")
            .1;
        let sentence = after_prefix
            .split_once(". Files are added")
            .expect("docs/corpus-format.md closes the topic-file list")
            .0;

        let tokens = extract_backticked_tokens(sentence);
        let mut documented = BTreeSet::new();
        for token in tokens {
            assert!(
                token.ends_with(".jsonl"),
                "unexpected non-.jsonl entry in docs/corpus-format.md topic list: {token:?}"
            );
            assert!(
                documented.insert(token.to_owned()),
                "duplicate topic file in docs/corpus-format.md: {token:?}"
            );
        }

        assert_eq!(
            documented, discovered,
            "docs/corpus-format.md topic file list must match corpus/*.jsonl on disk"
        );
    }

    /// The pinned `mdtheme` CLI version lives in `mise.toml`. Both README
    /// theme docs restate it, so keep them aligned with the tool manifest.
    #[test]
    fn mdtheme_version_in_docs_matches_mise_toml() {
        let repository_root = repository_root();
        let mise =
            fs::read_to_string(repository_root.join("mise.toml")).expect("mise.toml is readable");
        let version = mise_tool_version(&mise, "github:sebastian-software/mdtheme")
            .expect("mise.toml pins github:sebastian-software/mdtheme");
        let expected_phrase = format!("The project pins mdtheme {version}");

        for path in [
            "docs/readme-theme.md",
            "docs/adr/readme-theme-composition.md",
        ] {
            let document = fs::read_to_string(repository_root.join(path))
                .unwrap_or_else(|error| panic!("{path} is readable: {error}"));
            let normalized = normalize_whitespace(&document);
            assert!(
                normalized.contains(&expected_phrase),
                "{path} must state the mise.toml mdtheme version ({expected_phrase:?})"
            );
            for mentioned in mdtheme_versions(&normalized) {
                assert_eq!(
                    mentioned, version,
                    "{path} mentions stale mdtheme version {mentioned:?}; mise.toml pins {version:?}"
                );
            }
        }
    }

    /// Every Git theme entry in `mdtheme.yaml` and `FERRAMENTA_PIN` in
    /// `scripts/readme-family-block.sh` must pin an exact 40-character
    /// lowercase hex commit SHA rather than a moving branch or tag.
    ///
    /// The two `sebastian-software/ferramenta` pins are intentionally
    /// distinct: `ferramenta-family` 2.0+ dropped the `ferramenta-readme` CLI
    /// binary used by `scripts/readme-family-block.sh` for published crate
    /// READMEs, so that script stays on the last pre-2.0 CLI revision while
    /// `mdtheme.yaml` pins the `mdtheme` frame revision (PR #467).
    #[test]
    fn readme_theme_and_family_block_pins_are_exact_commit_shas() {
        let repository_root = repository_root();
        let mdtheme = fs::read_to_string(repository_root.join("mdtheme.yaml"))
            .expect("mdtheme.yaml is readable");
        let git_themes = mdtheme_git_theme_refs(&mdtheme);
        assert!(
            !git_themes.is_empty(),
            "mdtheme.yaml must declare at least one git theme"
        );
        for (url, reference) in git_themes {
            let pin = reference
                .unwrap_or_else(|| panic!("mdtheme.yaml git theme {url} must declare a ref"));
            assert!(
                is_commit_sha(pin),
                "mdtheme.yaml ref for {url} must be a 40-char lowercase hex commit SHA: {pin:?}"
            );
        }

        let script = fs::read_to_string(repository_root.join("scripts/readme-family-block.sh"))
            .expect("scripts/readme-family-block.sh is readable");
        let script_pins = script
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("FERRAMENTA_PIN=\"")?
                    .strip_suffix('"')
            })
            .collect::<Vec<_>>();
        assert_eq!(
            script_pins.len(),
            1,
            "scripts/readme-family-block.sh must declare FERRAMENTA_PIN exactly once"
        );
        assert!(
            is_commit_sha(script_pins[0]),
            "FERRAMENTA_PIN in scripts/readme-family-block.sh must be a 40-char lowercase hex commit SHA: {:?}",
            script_pins[0]
        );
    }

    /// `homepage/samples/Cargo.toml` depends on the published crates from
    /// crates.io (and can only be raised after a release reaches crates.io, so
    /// it deliberately stays independent of an in-flight Release Please bump).
    /// Within the homepage sample pipeline, however, the manifest pins for
    /// `ferralk` and `ferralk-glob`, `homepage/samples/Cargo.lock`, and the
    /// `renderedWith.ferralk` field in `homepage/app/data/home-samples.json`
    /// must always agree on one exact version.
    #[test]
    fn homepage_samples_manifest_lockfile_and_rendered_json_agree() {
        let repository_root = repository_root();
        let manifest = fs::read_to_string(repository_root.join("homepage/samples/Cargo.toml"))
            .expect("homepage/samples/Cargo.toml is readable");
        let ferralk_pin = exact_dependency_pin(&manifest, "ferralk")
            .expect("homepage/samples/Cargo.toml pins ferralk with =<version>");
        let ferralk_glob_pin = exact_dependency_pin(&manifest, "ferralk-glob")
            .expect("homepage/samples/Cargo.toml pins ferralk-glob with =<version>");
        assert_eq!(
            ferralk_glob_pin, ferralk_pin,
            "homepage/samples/Cargo.toml must pin ferralk and ferralk-glob to the same version"
        );

        let lockfile = fs::read_to_string(repository_root.join("homepage/samples/Cargo.lock"))
            .expect("homepage/samples/Cargo.lock is readable");
        for name in ["ferralk", "ferralk-glob"] {
            let entry = format!("name = \"{name}\"");
            let version = lockfile
                .lines()
                .skip_while(|line| *line != entry)
                .nth(1)
                .unwrap_or_else(|| panic!("homepage/samples/Cargo.lock locks {name}"));
            assert_eq!(
                version,
                format!("version = \"{ferralk_pin}\""),
                "homepage/samples/Cargo.lock must lock {name} at {ferralk_pin}"
            );
        }

        let samples_json =
            fs::read_to_string(repository_root.join("homepage/app/data/home-samples.json"))
                .expect("homepage/app/data/home-samples.json is readable");
        let samples: Value = serde_json::from_str(&samples_json)
            .expect("homepage/app/data/home-samples.json is valid JSON");
        assert_eq!(
            samples["renderedWith"]["ferralk"].as_str(),
            Some(ferralk_pin),
            "homepage/app/data/home-samples.json renderedWith.ferralk must match homepage/samples/Cargo.toml"
        );
    }

    /// The coverage floor is declared once, as `COVERAGE_MIN_LINES` in the
    /// `coverage` job. Two documents restate it for readers, so this contract
    /// holds them to the workflow instead of trusting them to be updated.
    ///
    /// Every comparison here is line by line, like the contracts above. The
    /// repository has no `.gitattributes`, so a Windows checkout carries CRLF
    /// and a needle containing a newline would match only on Unix.
    #[test]
    fn the_coverage_floor_is_declared_once_and_restated_consistently() {
        let repository_root = repository_root();
        let workflow = fs::read_to_string(repository_root.join(".github/workflows/ci.yml"))
            .expect("CI workflow is readable");

        let declarations = workflow
            .lines()
            .filter_map(|line| line.trim().strip_prefix("COVERAGE_MIN_LINES:"))
            .map(str::trim)
            .collect::<Vec<_>>();
        assert_eq!(
            declarations.len(),
            1,
            "the coverage floor must be declared exactly once, as COVERAGE_MIN_LINES"
        );
        let floor = declarations[0];
        assert!(
            workflow
                .lines()
                .any(|line| line.contains("--fail-under-lines \"$COVERAGE_MIN_LINES\"")),
            "the coverage job must gate on COVERAGE_MIN_LINES, not on a literal floor"
        );

        let readme =
            fs::read_to_string(repository_root.join("README.md")).expect("README is readable");
        let badge = readme
            .lines()
            .find(|line| line.contains("img.shields.io/badge/coverage%20gate"))
            .expect("the README carries the coverage-gate badge");
        assert!(
            badge.contains(&format!("\u{2265} {floor}%")),
            "the README coverage badge must read {floor}% in its link text: {badge}"
        );
        assert!(
            badge.contains(&format!("%20{floor}%25")),
            "the README coverage badge must render {floor}% in its image: {badge}"
        );

        let contributing = fs::read_to_string(repository_root.join("CONTRIBUTING.md"))
            .expect("CONTRIBUTING is readable");
        let expected_argument = format!("--fail-under-lines {floor}");
        assert!(
            contributing
                .lines()
                .any(|line| line.trim().ends_with(&expected_argument)),
            "the local coverage command in CONTRIBUTING must use the {floor}% floor"
        );
    }

    /// The nightly toolchain is pinned once, in `.github/nightly-toolchain`.
    /// Every lane that needs a nightly reads that file into
    /// `NIGHTLY_TOOLCHAIN`, so a workflow restating a dated nightly is the
    /// second copy this contract exists to prevent.
    #[test]
    fn the_nightly_toolchain_is_pinned_once() {
        let repository_root = repository_root();
        let pin = fs::read_to_string(repository_root.join(".github/nightly-toolchain"))
            .expect("the nightly pin is readable");
        let pins = pin
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            pins.len(),
            1,
            ".github/nightly-toolchain must hold exactly one toolchain"
        );
        let date = pins[0]
            .strip_prefix("nightly-")
            .expect("the pin names a nightly toolchain");
        assert!(
            date.len() == 10
                && date.bytes().enumerate().all(|(index, byte)| match index {
                    4 | 7 => byte == b'-',
                    _ => byte.is_ascii_digit(),
                }),
            "the pin must be a dated nightly (nightly-YYYY-MM-DD), not a floating one: {}",
            pins[0]
        );

        let readers = [
            "ci.yml",
            "glob-fuzz.yml",
            "linux-native-fuzz.yml",
            "macos-native-fuzz.yml",
        ];
        let workflows = repository_root.join(".github/workflows");
        for entry in fs::read_dir(&workflows).expect("the workflow directory is readable") {
            let path = entry.expect("workflow entry is readable").path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("workflow file names are UTF-8")
                .to_owned();
            let workflow = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{name} is readable: {error}"));
            if let Some(line) = workflow.lines().find(|line| line.contains("nightly-20")) {
                panic!("{name} restates a dated nightly instead of reading the pin: {line}");
            }
            let reads_pin = workflow
                .lines()
                .any(|line| line.contains("$(cat .github/nightly-toolchain)"));
            let uses_nightly = workflow
                .lines()
                .any(|line| line.contains("NIGHTLY_TOOLCHAIN"));
            assert_eq!(
                reads_pin, uses_nightly,
                "{name} must read NIGHTLY_TOOLCHAIN from .github/nightly-toolchain"
            );
            assert!(
                !readers.contains(&name.as_str()) || reads_pin,
                "{name} runs a nightly lane and must read .github/nightly-toolchain"
            );
        }
    }

    /// `ferralk` is the root package while its sources live under
    /// `crates/ferralk/`, so Cargo never auto-discovers files placed in
    /// `crates/ferralk/tests/` or `crates/ferralk/examples/`. Every integration
    /// test and example must be registered in root `Cargo.toml`, or it compiles
    /// and runs nowhere.
    #[test]
    fn every_root_package_test_and_example_is_registered() {
        let repository_root = repository_root();
        let metadata = cargo_metadata(&repository_root);

        for (kind, directory) in [
            ("test", "crates/ferralk/tests"),
            ("example", "crates/ferralk/examples"),
        ] {
            let registered = package_target_paths(&metadata, "ferralk", kind);
            let discovered = cargo_target_entry_points(&repository_root, directory);
            assert_eq!(
                registered, discovered,
                "every {kind} entry point in {directory} must be registered as a [[{kind}]] target in root Cargo.toml"
            );
        }
    }

    #[test]
    fn semver_values_keep_prerelease_and_build_metadata() {
        assert_eq!(semver_values("ferralk = \"1.0.0\""), vec!["1.0.0"]);
        assert_eq!(
            semver_values("1.0.0-rc.1 release line for applications."),
            vec!["1.0.0-rc.1"]
        );
        assert_eq!(
            semver_values("Ferralk 1.0.0-rc.1 is published."),
            vec!["1.0.0-rc.1"]
        );
        assert_eq!(
            semver_values("ferralk = { version = \"1.0.0-rc.1\", features = [] }"),
            vec!["1.0.0-rc.1"]
        );
        assert_eq!(
            semver_values("2.3.4+build.5 shipped"),
            vec!["2.3.4+build.5"]
        );
        assert_eq!(
            semver_values("0.1.2-alpha.1+build.7, then 1.2.3"),
            vec!["0.1.2-alpha.1+build.7", "1.2.3"]
        );
        assert_eq!(semver_values("MSRV 1.96"), Vec::<&str>::new());
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

    #[test]
    fn doc_sync_helpers_parse_counts_pins_and_tokens() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(84), "84");
        assert_eq!(format_with_commas(994), "994");
        assert_eq!(format_with_commas(1_000), "1,000");
        assert_eq!(format_with_commas(1_123), "1,123");
        assert_eq!(format_with_commas(1_000_000), "1,000,000");

        assert_eq!(
            extract_backticked_tokens("`a.jsonl`, `b.jsonl`, and `c.jsonl`"),
            vec!["a.jsonl", "b.jsonl", "c.jsonl"]
        );
        assert_eq!(
            mdtheme_versions("The project pins mdtheme 0.4.1 and runs mdtheme 0.4.1."),
            vec!["0.4.1", "0.4.1"]
        );
        assert_eq!(
            mdtheme_versions("mdtheme owns the README and pins mdtheme 0.4.1"),
            vec!["0.4.1"]
        );
        assert_eq!(
            mise_tool_version(
                "[tools.\"github:sebastian-software/mdtheme\"]\nversion = \"0.4.1\"\n[settings]\n",
                "github:sebastian-software/mdtheme"
            ),
            Some("0.4.1")
        );
        assert_eq!(
            mdtheme_git_theme_refs(
                "themes:\n  \
                 - git: https://example.com/a.git\n    \
                   ref: abcdef0123456789abcdef0123456789abcdef01\n  \
                 - git: https://example.com/b.git\n    \
                   ref: main\n  \
                 - path: markdown\n    \
                   git: https://example.com/reordered.git\n    \
                   ref: \"0123456789abcdef0123456789abcdef01234567\" # pinned\n  \
                 - path: local-only\n  \
                 - ref: fedcba9876543210fedcba9876543210fedcba98\n    \
                   git: https://example.com/ref-first.git\n  \
                 - path: markdown\n    \
                   git: https://example.com/missing-ref.git\n"
            ),
            vec![
                (
                    "https://example.com/a.git",
                    Some("abcdef0123456789abcdef0123456789abcdef01")
                ),
                ("https://example.com/b.git", Some("main")),
                (
                    "https://example.com/reordered.git",
                    Some("0123456789abcdef0123456789abcdef01234567")
                ),
                (
                    "https://example.com/ref-first.git",
                    Some("fedcba9876543210fedcba9876543210fedcba98")
                ),
                ("https://example.com/missing-ref.git", None),
            ]
        );
        assert!(is_commit_sha("abcdef0123456789abcdef0123456789abcdef01"));
        assert!(!is_commit_sha("main"));
        assert!(!is_commit_sha("ABCDEF0123456789ABCDEF0123456789ABCDEF01"));
        assert_eq!(
            exact_dependency_pin(
                "[dependencies]\nferralk = \"=1.2.1\"\nferralk-glob = \"=1.2.1\"\n",
                "ferralk"
            ),
            Some("1.2.1")
        );
    }

    fn repository_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn release_please_config(repository_root: &Path) -> Value {
        let release_please =
            fs::read_to_string(repository_root.join(".release-please-config.json"))
                .expect("Release Please configuration is readable");
        serde_json::from_str(&release_please).expect("Release Please configuration is JSON")
    }

    /// The product version: the root `ferralk` package's own `version`, which
    /// Release Please's `rust` strategy rewrites on every release.
    fn product_version(repository_root: &Path) -> String {
        let manifest = fs::read_to_string(repository_root.join("Cargo.toml"))
            .expect("workspace manifest is readable");
        package_version(&manifest)
            .map(str::to_owned)
            .expect("the root package declares a concrete version")
    }

    /// The workspace MSRV declared under `[workspace.package].rust-version`
    /// in the root `Cargo.toml`.
    fn workspace_rust_version(repository_root: &Path) -> String {
        let manifest = fs::read_to_string(repository_root.join("Cargo.toml"))
            .expect("workspace manifest is readable");
        manifest
            .lines()
            .skip_while(|line| line.trim() != "[workspace.package]")
            .skip(1)
            .take_while(|line| !line.trim_start().starts_with('['))
            .find_map(|line| {
                line.trim()
                    .strip_prefix("rust-version = \"")
                    .and_then(|value| value.strip_suffix('\"'))
            })
            .map(str::to_owned)
            .expect("root Cargo.toml declares [workspace.package].rust-version")
    }

    /// The concrete `version` in a manifest's `[package]` table, or `None`
    /// when it is missing or inherited from the workspace.
    fn package_version(manifest: &str) -> Option<&str> {
        manifest
            .lines()
            .skip_while(|line| line.trim() != "[package]")
            .skip(1)
            .take_while(|line| !line.trim_start().starts_with('['))
            .find_map(|line| {
                line.trim()
                    .strip_prefix("version = \"")
                    .and_then(|value| value.strip_suffix('\"'))
            })
    }

    fn normalize_whitespace(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn format_with_commas(value: usize) -> String {
        let digits = value.to_string();
        let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
        for (index, byte) in digits.bytes().enumerate() {
            let remaining = digits.len() - index;
            if index > 0 && remaining.is_multiple_of(3) {
                formatted.push(',');
            }
            formatted.push(char::from(byte));
        }
        formatted
    }

    fn corpus_topic_files(repository_root: &Path) -> BTreeSet<String> {
        let corpus_dir = repository_root.join("corpus");
        let mut files = BTreeSet::new();
        for entry in fs::read_dir(&corpus_dir).expect("corpus/ directory is readable") {
            let path = entry.expect("corpus/ entry is readable").path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("corpus topic file names are UTF-8")
                    .to_owned();
                files.insert(name);
            }
        }
        files
    }

    fn corpus_case_and_divergence_counts(repository_root: &Path) -> (usize, usize) {
        let corpus_dir = repository_root.join("corpus");
        let mut total_cases = 0_usize;
        let mut divergences = 0_usize;
        for name in corpus_topic_files(repository_root) {
            let contents = fs::read_to_string(corpus_dir.join(&name))
                .unwrap_or_else(|error| panic!("corpus/{name} is readable: {error}"));
            for (line_index, line) in contents.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("corpus/{name}:{} is valid JSON: {error}", line_index + 1)
                });
                let object = value
                    .as_object()
                    .unwrap_or_else(|| panic!("corpus/{name}:{} is a JSON object", line_index + 1));
                total_cases += 1;
                if object.contains_key("oracle_expected")
                    || object.contains_key("oracle_matches")
                    || object.contains_key("oracle_indices")
                {
                    divergences += 1;
                }
            }
        }
        (total_cases, divergences)
    }

    fn extract_backticked_tokens(text: &str) -> Vec<&str> {
        let mut tokens = Vec::new();
        let mut remainder = text;
        while let Some((_, after_open)) = remainder.split_once('`') {
            let (token, after_close) = after_open
                .split_once('`')
                .expect("every backticked token in corpus-format.md is closed");
            tokens.push(token);
            remainder = after_close;
        }
        tokens
    }

    fn mise_tool_version<'a>(mise: &'a str, tool: &str) -> Option<&'a str> {
        let header = format!("[tools.\"{tool}\"]");
        mise.lines()
            .skip_while(|line| line.trim() != header)
            .skip(1)
            .take_while(|line| !line.trim_start().starts_with('['))
            .find_map(|line| {
                line.trim()
                    .strip_prefix("version = \"")
                    .and_then(|value| value.strip_suffix('\"'))
            })
    }

    fn mdtheme_versions(text: &str) -> Vec<&str> {
        let mut versions = Vec::new();
        let mut remainder = text;
        while let Some((_, after)) = remainder.split_once("mdtheme ") {
            if let Some(first) = after.as_bytes().first()
                && first.is_ascii_digit()
            {
                let end = after
                    .bytes()
                    .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
                    .count();
                let version = after[..end].trim_end_matches('.');
                if !version.is_empty() {
                    versions.push(version);
                }
                remainder = &after[end..];
            } else {
                remainder = after;
            }
        }
        versions
    }

    fn is_commit_sha(value: &str) -> bool {
        value.len() == 40
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    fn mdtheme_git_theme_refs(mdtheme_yaml: &str) -> Vec<(&str, Option<&str>)> {
        let mut results = Vec::new();
        let mut in_themes = false;
        let mut current_git: Option<&str> = None;
        let mut current_ref: Option<&str> = None;

        for line in mdtheme_yaml.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = line.bytes().take_while(|byte| *byte == b' ').count();
            if indent == 0 && !trimmed.starts_with('-') {
                if let Some(url) = current_git.take() {
                    results.push((url, current_ref.take()));
                }
                current_ref = None;
                in_themes = trimmed
                    .split_once(':')
                    .is_some_and(|(key, _)| key.trim() == "themes");
                continue;
            }
            if !in_themes {
                continue;
            }

            let kv_line = if trimmed == "-" {
                if let Some(url) = current_git.take() {
                    results.push((url, current_ref.take()));
                }
                current_ref = None;
                continue;
            } else if let Some(rest) = trimmed.strip_prefix("- ") {
                if let Some(url) = current_git.take() {
                    results.push((url, current_ref.take()));
                }
                current_ref = None;
                rest.trim()
            } else {
                trimmed
            };

            if let Some((key, value)) = kv_line.split_once(':') {
                let scalar = parse_yaml_scalar(value);
                if scalar.is_empty() {
                    continue;
                }
                match key.trim() {
                    "git" => current_git = Some(scalar),
                    "ref" => current_ref = Some(scalar),
                    _ => {}
                }
            }
        }
        if let Some(url) = current_git {
            results.push((url, current_ref));
        }

        results
    }

    fn parse_yaml_scalar(raw: &str) -> &str {
        let trimmed = raw.trim();
        for quote in ['"', '\''] {
            if let Some(rest) = trimmed.strip_prefix(quote)
                && let Some((quoted, _)) = rest.split_once(quote)
            {
                return quoted;
            }
        }
        let without_comment = trimmed
            .find(" #")
            .or_else(|| trimmed.find("\t#"))
            .map_or(trimmed, |index| &trimmed[..index]);
        without_comment.trim()
    }

    fn exact_dependency_pin<'a>(manifest: &'a str, dependency: &str) -> Option<&'a str> {
        let prefix = format!("{dependency} = \"=");
        manifest
            .lines()
            .skip_while(|line| line.trim() != "[dependencies]")
            .skip(1)
            .take_while(|line| !line.trim_start().starts_with('['))
            .find_map(|line| line.trim().strip_prefix(&prefix)?.strip_suffix('\"'))
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
            if patch_start == index {
                continue;
            }
            index = consume_semver_suffixes(bytes, index);
            versions.push(&line[start..index]);
        }

        versions
    }

    /// Consumes an optional pre-release (`-rc.1`) and build-metadata
    /// (`+build.5`) suffix that follows a `<major>.<minor>.<patch>` triple.
    ///
    /// Release Please writes the whole version into the annotated consumer
    /// documentation lines, so during a pre-release train the annotation reads
    /// `1.0.0-rc.1`. Stopping after the patch component would compare the
    /// release line against the workspace version and fail on every
    /// `-rc`/`-beta` release.
    fn consume_semver_suffixes(bytes: &[u8], mut index: usize) -> usize {
        // A pre-release comes first, build metadata second; both optional.
        for separator in *b"-+" {
            if index < bytes.len()
                && bytes[index] == separator
                && let Some(end) = consume_dot_separated_identifiers(bytes, index + 1)
            {
                index = end;
            }
        }

        index
    }

    /// Returns the index just past the last complete `.`-separated SemVer
    /// identifier, or `None` when the run does not start with one. Trailing
    /// punctuation such as the full stop in `Ferralk 1.0.0-rc.1 is published.`
    /// therefore stays outside the version.
    fn consume_dot_separated_identifiers(bytes: &[u8], start: usize) -> Option<usize> {
        let mut index = start;
        let mut end = None;

        loop {
            let identifier_start = index;
            while index < bytes.len() && is_semver_identifier_byte(bytes[index]) {
                index += 1;
            }
            if index == identifier_start {
                return end;
            }
            end = Some(index);
            if index < bytes.len() && bytes[index] == b'.' {
                index += 1;
                continue;
            }
            return end;
        }
    }

    fn is_semver_identifier_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'-'
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
            } else if path.extension().is_some_and(|extension| extension == "md")
                && !is_generated_site_document(root, &path)
            {
                discovered.insert(
                    path.strip_prefix(root)
                        .expect("documentation file is below the docs root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }

    // Generated website routes mirror the canonical documents already checked
    // above. The handwritten guide remains in the inventory and compiles here.
    fn is_generated_site_document(root: &Path, path: &Path) -> bool {
        let relative = path
            .strip_prefix(root)
            .expect("site file is below the root");
        relative.starts_with("homepage/app/routes")
    }

    fn is_excluded_directory(root: &Path, directory: &Path) -> bool {
        // Git metadata and generated/build trees are not repository
        // documentation. Every other Markdown file is inventory-controlled.
        //
        // `node_modules` is here because the Node benchmark harness is a
        // documented lane: `npm ci` under `tools/bench/node` drops dozens of
        // dependency READMEs into the working tree, and without this the next
        // `cargo test --workspace` fails with an inventory mismatch for files
        // the repository's own `.gitignore` excludes.
        let relative = directory
            .strip_prefix(root)
            .expect("repository directory is below the repository root");
        if relative.starts_with(Path::new(".claude").join("worktrees"))
            || relative.starts_with("homepage/build")
        {
            return true;
        }
        relative.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(".git" | "target" | "vendor" | "node_modules")
            )
        })
    }

    fn cargo_metadata(repository_root: &Path) -> Value {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = std::process::Command::new(cargo)
            .args([
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(repository_root.join("Cargo.toml"))
            .output()
            .expect("cargo metadata executes");
        assert!(
            output.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("cargo metadata outputs JSON")
    }

    fn package_target_paths(metadata: &Value, package_name: &str, kind: &str) -> BTreeSet<String> {
        let workspace_root = Path::new(
            metadata["workspace_root"]
                .as_str()
                .expect("cargo metadata reports workspace_root"),
        );
        let package = metadata["packages"]
            .as_array()
            .expect("cargo metadata lists packages")
            .iter()
            .find(|package| package["name"].as_str() == Some(package_name))
            .unwrap_or_else(|| panic!("cargo metadata includes package {package_name}"));
        let mut paths = BTreeSet::new();
        for target in package["targets"]
            .as_array()
            .expect("package metadata lists targets")
        {
            let kinds = target["kind"]
                .as_array()
                .expect("target metadata lists kinds");
            if kinds.len() == 1 && kinds[0].as_str() == Some(kind) {
                let src_path = Path::new(
                    target["src_path"]
                        .as_str()
                        .expect("target metadata reports src_path"),
                );
                let relative = src_path
                    .strip_prefix(workspace_root)
                    .unwrap_or_else(|error| {
                        panic!(
                            "target path {} is below workspace root {}: {error}",
                            src_path.display(),
                            workspace_root.display()
                        )
                    })
                    .to_string_lossy()
                    .replace('\\', "/");
                assert!(
                    paths.insert(relative.clone()),
                    "duplicate {kind} target registration in Cargo.toml: {relative}"
                );
            }
        }
        paths
    }

    /// Discovers Cargo target entry points under `relative_directory`:
    /// top-level `*.rs` files and immediate `<subdir>/main.rs` entry points,
    /// ignoring helper modules inside subdirectories.
    fn cargo_target_entry_points(root: &Path, relative_directory: &str) -> BTreeSet<String> {
        let directory = root.join(relative_directory);
        let mut discovered = BTreeSet::new();
        for entry in fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("{relative_directory} is readable: {error}"))
        {
            let path = entry
                .unwrap_or_else(|error| panic!("{relative_directory} entry is readable: {error}"))
                .path();
            let entry_point =
                if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
                    Some(path)
                } else if path.is_dir() {
                    let main_rs = path.join("main.rs");
                    main_rs.is_file().then_some(main_rs)
                } else {
                    None
                };
            if let Some(entry_point) = entry_point {
                discovered.insert(
                    entry_point
                        .strip_prefix(root)
                        .expect("source file is below the repository root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        discovered
    }
}
