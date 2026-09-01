#![forbid(unsafe_code)]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ferralk-corpus-contract-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create corpus fixture");
        Self { root }
    }

    fn run(&self, file_name: &str, record: &str) -> Output {
        fs::write(self.root.join(file_name), format!("{record}\n")).expect("write corpus fixture");
        Command::new(env!("CARGO_BIN_EXE_harness"))
            .arg(&self.root)
            .output()
            .expect("run corpus harness")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove corpus fixture");
    }
}

fn assert_rejected(output: &Output, needle: &str) {
    assert!(!output.status.success(), "harness unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(needle), "missing {needle:?} in {stderr:?}");
}

fn run(file_name: &str, record: &str) -> Output {
    Fixture::new().run(file_name, record)
}

#[test]
fn list_expected_is_derived_from_the_real_selection() {
    let output = run(
        "list.jsonl",
        r#"{"id":"empty-list","kind":"match_paths","pattern":"*.txt","path":"","paths":[],"matches":[],"flags":[],"expected":true,"source":"handwritten"}"#,
    );
    assert_rejected(&output, "expected true, got false");
}

#[test]
fn absolute_expected_is_derived_from_the_real_rewrite() {
    let output = run(
        "absolute.jsonl",
        r#"{"id":"outside-root","kind":"absolute_pattern","pattern":"/other/**","path":"","base_path":"/repo","flags":[],"expected":true,"source":"handwritten"}"#,
    );
    assert_rejected(&output, "expected true, got false");
}

#[test]
fn every_record_requires_an_explicit_kind() {
    let output = run(
        "matcher.jsonl",
        r#"{"id":"missing-kind","pattern":"src/**/*.rs","path":"src/a/b/main.rs","flags":["recursive_double_star"],"expected":true,"source":"fast_glob"}"#,
    );
    assert_rejected(&output, "kind");
}

#[test]
fn fields_irrelevant_to_the_kind_are_rejected_even_when_empty() {
    let output = run(
        "matcher.jsonl",
        r#"{"id":"foreign-field","kind":"matcher","pattern":"*","path":"x","paths":[],"flags":[],"expected":true,"source":"handwritten"}"#,
    );
    assert_rejected(&output, "paths");
}

#[test]
fn schema_rejects_duplicate_flags() {
    let output = run(
        "matcher.jsonl",
        r#"{"id":"duplicate-flags","kind":"matcher","pattern":"*","path":"x","flags":["braces","braces"],"expected":true,"source":"handwritten"}"#,
    );
    assert_rejected(&output, "flags");
}

#[test]
fn schema_rejects_an_invalid_id() {
    let output = run(
        "matcher.jsonl",
        r#"{"id":"Invalid_ID","kind":"matcher","pattern":"*","path":"x","flags":[],"expected":true,"source":"handwritten"}"#,
    );
    assert_rejected(&output, "id");
}

#[test]
fn ignore_routing_depends_on_kind_instead_of_file_name() {
    let output = run(
        "renamed-topic.jsonl",
        r#"{"id":"renamed-ignore","kind":"ignore","pattern":"*.log","path":"debug.log","ignore_rules":["*.log"],"expected":true,"source":"git_check_ignore"}"#,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("deferred 1 ignore case"),
        "unexpected summary: {stdout:?}"
    );
}
