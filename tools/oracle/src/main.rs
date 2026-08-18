#![forbid(unsafe_code)]

//! The oracle is an ignored integration test, not a normal executable.
//!
//! Run `cargo test -p oracle --test zlob_oracle -- --ignored` only after
//! installing the pinned zlob build prerequisites (Zig 0.16 and libclang).
fn main() {
    eprintln!("run `cargo test -p oracle --test zlob_oracle -- --ignored`");
}
