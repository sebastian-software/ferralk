# Deferred follow-up

Ferralk `0.1.2` is published. There are no external release gates and no
numeric performance gates. The source of truth for remaining follow-up work
is GitHub:

| Topic | Tracking issue |
| --- | --- |
| Feedback from the Palamedes integration trial | [#13](https://github.com/sebastian-software/ferralk/issues/13) |

The native filesystem parity corpus that used to sit in this table closed with
[#12](https://github.com/sebastian-software/ferralk/issues/12): eleven parity
families now compare the native and portable backends over whole trees, on both
platforms and under the sanitizers.

## Platform state

| Platform | Backend | Exercised by |
| --- | --- | --- |
| macOS | `getattrlistbulk`, behind `native-macos` | macOS native and AddressSanitizer jobs, every pull request |
| Linux | `getdents64`, behind `native-linux` | Linux native, AddressSanitizer and Miri jobs, every pull request |
| Windows | Portable reader only | Windows test job, every pull request |

Both native backends degrade per entry rather than per directory, and fall back
to the portable reader when a filesystem cannot answer them; the fallback paths
carry their own tests. Windows is deliberately portable-only, including the
follow-symlinks cycle key, which needs a file index Rust does not expose on
stable there — [ADR-0005](adr/0005-byte-matching-wtf8-on-windows.md) and
[ADR-0010](adr/0010-portable-1.0-native-backends-macos-then-linux.md) carry the
reasoning.

The native validation workflows run automatically for pull requests. Their
purpose is safety and behavioural parity, not a performance threshold.

Benchmark evidence lives in [benchmark evidence](benchmark-evidence.md): what
each lane measures, how to reproduce it, and how ferralk compares with the Rust
baselines and with zlob. None of it gates a release.
