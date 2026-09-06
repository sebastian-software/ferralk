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
  Conventional Commit syntax, and a consumer-visible pre-1.0 behavior change
  needs both `!` and a filled-in `BREAKING CHANGE:` footer.
- Preserve byte-first path handling, explicit wildcard semantics, and the
  existing documentation and benchmark evidence requirements.

---

<!-- sebastian-software-consumer-agents:start -->

# Standards-managed repo guardrails

- Do not hand-edit managed files or standards-owned marker sections.
- If `standards check` reports drift, run `standards apply` or update standards.
- `pnpm agent:check` may omit `standards check`; CI can still fail on drift.
- Fix or format every file reported by `oxfmt` whenever practical.
- For generated files, prefer formatting in the generator step.
- If formatting is not viable, use repo-local `.prettierignore`.
- Never add repo-specific ignores to managed `.oxfmtrc.json`.
<!-- sebastian-software-consumer-agents:end -->
