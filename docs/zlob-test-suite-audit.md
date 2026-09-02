# Frozen zlob test-suite audit

This audit tracks the frozen zlob v1.6.3 test tree at commit
`4bc4da2cbc823d3911b4a1436448687c398977dd`. It distinguishes public Rust
semantics that Ferralk can replay locally from C ABI, loader, and
platform-runtime tests that are deliberately outside the two-crate Rust API
(ADR-0003). It is an M0 progress record, not a claim of C compatibility.

| Frozen source | Current Ferralk evidence | Disposition |
| --- | --- | --- |
| `test_fnmatch.zig`, `test_brace.zig`, `test_edge_cases.zig` | Source-linked matcher corpus and Zig-backed oracle | Covered for direct matcher semantics. |
| `test_extglob.zig`, `test_glibc.zig`, `test_basic.zig` | Path-list corpus and matcher regressions | Covered for public in-memory forms; filesystem enumeration stays separate. |
| `test_path_matcher.zig`, public `matchPaths` block in `test_internal.zig` | `filter_paths`, base-relative and index corpus cases | Covered; C-string chunking and fixed component buffers are excluded. |
| `test_absolute_paths.zig` | Root-independent list corpus plus provenance-marked C/Rust differences | Covered except the documented C iterator-only literal-hidden-brace case. |
| `test_gitignore.zig`, `test_gitignore_e2e.zig` | `ignore.jsonl`, Git oracle, Walker fixtures | Git is normative under ADR-0006; zlob's private parser is provenance. |
| `test_walk.zig` | Walker fixtures for filtering, depth, kind, basename, symlinks, Gitignore allowlisting, `.ignore` precedence, scoped-unreadable pruning, serial/parallel equivalence | Public Rust-shaped traversal contract is being ported incrementally; visitor callbacks and C error callbacks have no equivalent API. |
| `test_rust_glob.zig` | Walker regression fixture for literal, wildcard, class, nested, special-character, `./`, trailing-slash, and root-component patterns | Covered for root-relative traversal filtering; zlob's C-shaped result buffer plus `.`/`..` root entries remain outside the Walker API. |
| `test_posix.zig`, `test_append.zig`, `test_c_api.c`, `test_errfunc.zig` | No direct equivalent | C result buffers, offsets, append, callbacks, alternate directory callbacks, tilde expansion, and POSIX runtime behaviour are deliberately excluded by ADR-0003. |
| `test_dlopen_tls.zig`, `dlopen_consumer.zig`, `test_static_tls_budget.zig` | No direct equivalent | Dynamic-loading and TLS-budget tests apply to zlob's C/shared-library distribution, which Ferralk does not ship. |
| `test_libc_comparison.sh` | No direct equivalent | Shell/libc comparison is system-glob evidence, not Ferralk's API contract. |
| private helper portions of `test_internal.zig`, `test_utils.zig` | No direct equivalent | Internal SIMD, allocation, and helper invariants are implementation details; performance-sensitive equivalents require profiling-backed work. |

## Scope decision

On 2026-08-19, the Rust-only boundary in ADR-0003 was explicitly confirmed for
Ferralk 1.0. M0 is therefore complete for the public Rust-shaped test contract
mapped above. C ABI, loader/TLS, callback, C-result-buffer, and libc-shell
tests remain recorded non-goals, not incomplete ports. Any future compatibility
layer requires a new product decision and a separate test-plan extension.
