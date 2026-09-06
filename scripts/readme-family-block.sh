#!/usr/bin/env bash
# Render or verify the Ferramenta family block in every README this
# repository publishes.
#
# The block is generated from the family registry in
# sebastian-software/ferramenta, which is the single source of truth for the
# member list, the job strings and the links. Nothing between the
# `ferramenta-family` markers is hand-edited here: a registry change reaches
# this repository by bumping FERRAMENTA_PIN below and re-running `--write`.
#
# Usage:
#   scripts/readme-family-block.sh --check   # exit 1 on drift (CI)
#   scripts/readme-family-block.sh --write   # regenerate the blocks
#
# Needs Node 22.13 or newer and pnpm; the generator is fetched from Git, so
# the machine running it needs network access.

set -euo pipefail

# The generator commit in sebastian-software/ferramenta. Pinned rather than
# tracking `main` so a run is reproducible: the block CI blesses today is the
# one it blessed yesterday. See CONTRIBUTING.md for the bump procedure.
FERRAMENTA_PIN="d63a0b163ef3e5e68cd1c77e5c8871ac72c36b60"

# The `&path:` fragment is required. Without it pnpm installs the site rather
# than the package, and there is no `ferramenta-readme` binary to run.
GENERATOR="github:sebastian-software/ferramenta#${FERRAMENTA_PIN}&path:/packages/ardo-config"

# The registry names this repository `ferralk`; the tool bolds that entry in
# the GitHub tables and leaves it out of its own sibling list.
CURRENT_TOOL="ferralk"

mode="${1:---check}"
case "$mode" in
  --check | --write) ;;
  *)
    printf 'usage: %s [--check|--write]\n' "$0" >&2
    exit 2
    ;;
esac

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

# The root README carries the full block: a sentence and one table per family
# group, placed above the standards-owned branding footer. The two published
# crate READMEs are what crates.io renders, so they carry the `registry`
# variant instead: two plain-Markdown lines, no HTML and no tables.
render() {
  local variant="$1"
  local readme="$2"
  pnpm dlx "$GENERATOR" \
    --current "$CURRENT_TOOL" --variant "$variant" "$mode" "$readme"
}

render github README.md
render registry crates/ferralk/README.md
render registry crates/ferralk-glob/README.md
