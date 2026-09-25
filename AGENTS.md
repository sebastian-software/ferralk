# Repository guidance

- `CONTRIBUTING.md#before-opening-a-pull-request` is the single canonical,
  Zig-free preflight. Run it as written and do not copy its command list into
  another document. The development-only `oracle` package requires Zig 0.16
  and libclang; CI installs them for its additional coverage lane.
- Read `docs/corpus-format.md` before adding or changing behavioral cases. The
  checked-in JSONL corpus is the source of truth for portable and oracle
  parity.
- Read `docs/adr/README.md` before proposing architectural changes. Accepted
  ADRs are project constraints, not decisions to re-litigate in routine work.
- Test `native-macos` changes on macOS and `native-linux` changes on Linux. The
  corresponding CI jobs cover the other platform and compile its cfg-gated
  fuzz targets.
- Follow `CONTRIBUTING.md#communicate-pre-10-contract-changes`: PR titles use
  Conventional Commit syntax. Since 1.0 the contract in `docs/stability.md` is
  in force: a consumer-visible change to it needs `!` and a filled-in
  `BREAKING CHANGE:` footer and means a major release.
- Preserve byte-first path handling, explicit wildcard semantics, and the
  existing documentation and benchmark evidence requirements.
