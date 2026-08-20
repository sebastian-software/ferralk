# ADR-0006: Git-normative ignore semantics

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

Git, the `ignore` crate (ripgrep), and zlob disagree in gitignore corner cases
(negation precedence, anchoring, `**` inside ignore rules). One of them has to
be the reference the corpus treats as correct.

## Decision

Git itself is normative: `git check-ignore` verdicts are the corpus oracle for
ignore semantics — this is the ground truth every user intuitively expects
("behaves like Git"). The `ignore` crate's Gitignore rule matcher is the
engine (its parallel walker is not reused, see ADR-0009). Where the engine
diverges from Git, the divergence is documented and we decide case by case
whether to patch. zlob's own ignore quirks are explicitly not inherited.

## Consequences

- User-intuitive behavior, testable against a universally available oracle.
- Occasional maintenance cost when the `ignore` crate and Git disagree.
- One more deliberate divergence class from zlob to document.

## Amendment, 2026-08-20

The engine half of this decision is superseded by
[ADR-0014](0014-own-gitignore-rule-matching.md): rule matching moves from the
`ignore` crate to a ferralk layer over ferralk-glob, because the borrowed
engine cannot reproduce Git's POSIX classes and the evaluation around it is
already ours. Git stays normative and `git check-ignore` stays the oracle —
that half is unchanged, and the maintenance cost named above turned out to be
the reason to stop paying it.

Amendment only; the normative-oracle decision above is unchanged.
