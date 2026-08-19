# Proposed note to zlob's maintainer

**Status:** draft only — it has not been sent.

**Subject:** Ferralk: an independently maintained Rust port inspired by zlob

Hi Dmitry,

I wanted to let you know about [Ferralk](https://github.com/sebastian-software/ferralk),
an independently maintained Rust implementation of the public Rust-shaped
matching and walking behaviour documented by zlob 1.6.3.

Ferralk retains attribution and provenance for code derived from the frozen
reference, and deliberately keeps its scope Rust-only: it does not reproduce
the C ABI, dynamic-loading, callback, or result-buffer surfaces. Its
compatibility guide documents the intended mapping and deliberate differences.

We have also kept zlob as a comparison oracle in the test and benchmark suite,
so behavioural and performance differences can be made visible rather than
being implied away.

I wanted to give you a courteous heads-up and thank you for the work that made
the comparison possible. If you have feedback on the attribution or the stated
compatibility boundary, it would be very welcome.

Best,

Sebastian

## Sending checklist

- Have a Ferralk maintainer review the wording and recipient/channel.
- Send only after that approval; do not treat this draft or its Git history as
  contact with zlob's maintainer.
- Record the date and link to the sent message in `MILESTONES.md` without
  copying private correspondence into the repository.
