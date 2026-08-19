# Deferred follow-up

Ferralk `0.1.2` is published. There are no external release gates and no
numeric performance gates. The source of truth for remaining follow-up work
is GitHub:

| Topic | Tracking issue |
| --- | --- |
| Broader native filesystem parity corpus | [#12](https://github.com/sebastian-software/ferralk/issues/12) |
| Feedback from the Palamedes integration trial | [#13](https://github.com/sebastian-software/ferralk/issues/13) |

The native validation workflows run automatically for pull requests. Their
purpose is safety and behavioural parity, not a performance threshold.

Benchmark evidence lives in [benchmark evidence](benchmark-evidence.md): what
each lane measures, how to reproduce it, and how ferralk compares with the Rust
baselines and with zlob. None of it gates a release.
