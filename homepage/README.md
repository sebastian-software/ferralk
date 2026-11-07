# Ferralk website

The Ardo 6 site at [ferralk.dev](https://ferralk.dev). Every page wears the
Monografie header and footer from `ferramenta-family/monografie`
(`monografie.css`); the homepage reads on the Monografie page, and the
documentation keeps the Ferra reading layout from `ferramenta-family/ferra`
(`ferra.css`, `ferra-ardo.css`) with Ardo's drawer and search as header actions.
Shared visual changes belong in the family package. The site styles only its
homepage specimens and shell seams, in CSS files beside their components under
`app/components/`, every selector scoped to an `fk-` class; the build checks
that.

From this directory:

```sh
pnpm install --frozen-lockfile
pnpm dev
pnpm verify
pnpm preview
```

`pnpm verify` checks the homepage samples, lint, formatting, types, the
production build, internal links and fragments, and every page at eight
viewport widths with accessibility and interaction checks. It needs the
repository's Rust toolchain and Playwright Chromium (`pnpm exec playwright
install chromium`).

`scripts/site-pages.mjs` maps repository documents to public URLs.
`scripts/write-site-data.mjs` generates ignored Markdown routes from those
canonical sources, rewrites relative repository links, and derives the release
and minimum Rust version. Edit the original documents in `docs/`, rather than
their generated routes. The handwritten introduction lives at
`content/getting-started.md`.

Everything the homepage shows is generated into the committed
`app/data/home-samples.json` by `scripts/render-home-samples.mjs`; nobody edits
it by hand:

- The matcher, walk and byte-name specimens are real output of `samples/`, a
  small program that depends on the published `ferralk` and `ferralk-glob`
  crates from crates.io at the exact versions pinned in `samples/Cargo.toml`
  (its own Cargo workspace, outside the repository's). The JSON records that
  version. The walk builds its project in a temporary directory and needs a
  Unix filesystem and a user other than root. After a release reaches
  crates.io, raise both pins and run `pnpm sample:write`.
- The speed comparison comes from the published tables in
  `docs/benchmark-evidence.md`, with the date, machine, revision and Ferralk
  version recorded there.

`pnpm sample:check` fails when the committed file no longer matches. The corpus
counts change with every new case, so `write-site-data.mjs` derives them from
`corpus/*.jsonl` at build time instead. The browser switches between the
recorded results and does not run Rust in the visitor's browser.

GitHub Actions checks pull requests and deploys `main` to the existing Pages
site. The `CNAME` remains `ferralk.dev`.
