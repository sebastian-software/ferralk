#![forbid(unsafe_code)]
#![doc = "Compile-checked Markdown documentation for Ferralk."]

#[cfg(test)]
const DOCUMENTS: &[&str] = &[
    "README.md",
    "benchmark-evidence.md",
    "usage.md",
    "compatibility-guide.md",
    "compatibility-matrix.md",
    "corpus-format.md",
    "external-release-gates.md",
    "fast-glob-reference.md",
    "palamedes-adoption.md",
    "zlob-1.6.3-reference.md",
    "zlob-fnmatch-test-coverage.md",
    "zlob-test-suite-audit.md",
    "adr/README.md",
    "adr/0001-independent-port-under-the-ferralk-name.md",
    "adr/0002-hybrid-port-strategy.md",
    "adr/0003-two-published-crates-no-c-abi.md",
    "adr/0004-msrv-stable-minus-two.md",
    "adr/0005-byte-matching-wtf8-on-windows.md",
    "adr/0006-git-normative-ignore-semantics.md",
    "adr/0007-differential-corpus-and-dev-time-oracle.md",
    "adr/0008-simd-via-memchr-primitives.md",
    "adr/0009-own-work-stealing-scheduler.md",
    "adr/0010-portable-1.0-native-backends-macos-then-linux.md",
    "adr/0011-posix-conservative-walker-defaults.md",
    "adr/0012-ferroni-repository-blueprint.md",
    "adr/0013-no-glob-to-regex-translation.md",
    "adr/0014-own-gitignore-rule-matching.md",
];

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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

    use super::DOCUMENTS;

    #[test]
    fn every_markdown_document_is_included() {
        let documents_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
        let mut discovered = BTreeSet::new();
        collect_markdown_files(&documents_root, &documents_root, &mut discovered);

        let included = DOCUMENTS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            included, discovered,
            "add each new Markdown document to this doctest harness"
        );
    }

    fn collect_markdown_files(root: &Path, directory: &Path, discovered: &mut BTreeSet<String>) {
        for entry in fs::read_dir(directory).expect("documentation directory is readable") {
            let entry = entry.expect("documentation directory entry is readable");
            let path = entry.path();
            if path.is_dir() {
                collect_markdown_files(root, &path, discovered);
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
}
