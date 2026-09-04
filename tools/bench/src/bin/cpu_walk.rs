#![forbid(unsafe_code)]
//! The user-space CPU harness: one walk, measured by instruction count.
//!
//! Wall time on a shared runner cannot see a walk that does much more
//! user-space work while the filesystem and the kernel hide it. On macOS that
//! blindness is near-total — 95% of a warm walk's samples are in `openat`,
//! `getdirentries64` and `close`, so a regression could triple the walker's
//! own work and barely move the median. This harness answers the question
//! wall time cannot: how many instructions did the walk execute?
//!
//! It is not a speed measurement and must never be read as one. Callgrind
//! serializes threads, so the four-thread arm reports the work a parallel walk
//! performs, not the time it takes; a change that moved work between threads
//! without removing any would look identical here. Elapsed time stays in
//! `walker.rs`.
//!
//! Two subcommands, because the fixture must not be inside the measured
//! region:
//!
//! ```text
//! cpu_walk prepare              # builds the tree natively, prints its path
//! cpu_walk walk <root> <threads>  # one walk, run this one under Callgrind
//! ```
//!
//! The walk asserts its exact result count, so a "faster" run that stopped
//! finding files fails instead of reporting an improvement.

use std::process::ExitCode;

use bench::{RepositoryFixture, TYPESCRIPT_PATTERN};
use ferralk::{WalkOptions, Walker};

/// What the repository fixture's unscoped TypeScript query selects. Hard-coded
/// so a walk that silently stops finding files cannot report fewer
/// instructions as progress.
const EXPECTED_MATCHES: usize = 7_400;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("prepare") => {
            let fixture = RepositoryFixture::new();
            assert_eq!(fixture.files(), 53_601, "the fixture shape changed");
            println!("{}", fixture.keep().display());
            ExitCode::SUCCESS
        }
        Some("walk") => {
            let Some(root) = arguments.get(1) else {
                eprintln!("walk needs a fixture root");
                return ExitCode::FAILURE;
            };
            let threads: usize = arguments
                .get(2)
                .map_or(Ok(1), |value| value.parse())
                .expect("thread count is a number");
            let found = Walker::new(root)
                .threads(threads)
                .include(TYPESCRIPT_PATTERN)
                .expect("harness include is valid")
                .options(WalkOptions::default())
                .collect()
                .expect("harness walk succeeds")
                .entries()
                .len();
            assert_eq!(
                found, EXPECTED_MATCHES,
                "the walk selected a different set, so its instruction count is not comparable"
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: cpu_walk prepare | cpu_walk walk <root> <threads>");
            ExitCode::FAILURE
        }
    }
}
