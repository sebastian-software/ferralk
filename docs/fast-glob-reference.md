# Oxc fast-glob reference

The second matcher reference is the Rust crate `fast-glob` `1.1.0`, the
version selected by Oxc's workspace at source commit
`8783524015b1e6ff1c39ccf426df0bb07cbbc588` (verified 2026-08-18). Its
upstream repository is `https://github.com/oxc-project/fast-glob`.

The harness runs only `corpus/fast-glob.jsonl` against this reference. Those
cases must use syntax shared by both engines and record a deliberate
disagreement with `oracle_expected` when needed. This is not a replacement for
the zlob oracle: fast-glob's documented `*` separator rule, leading `!`
negation, validation model, and maximum brace nesting differ from zlob and
from ferralk's documented compatibility profile.
