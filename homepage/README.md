# Ferralk website

The Ardo 6 site at [ferralk.dev](https://ferralk.dev) consumes the shared
`ferramenta-family/ferra` components and styles. It has no project stylesheet.

From this directory:

```sh
pnpm install --frozen-lockfile
pnpm dev
pnpm verify
pnpm preview
```

`pnpm verify` checks the native Rust example, lint, formatting, types, the
production build, internal links and fragments, and every page at eight
viewport widths with accessibility and interaction checks. It needs the
repository's Rust toolchain and Playwright Chromium (`pnpm exec playwright
install chromium`).

`scripts/site-pages.mjs` maps repository documents to public URLs.
`scripts/write-site-data.mjs` generates ignored Markdown routes from those
canonical sources, rewrites relative repository links, and derives release
and benchmark data. Edit the original documents in `docs/`, rather than their
generated routes. The handwritten introduction lives at
`content/getting-started.md`.

The homepage's file-search choices are real runs of the existing Rust `find`
example over a temporary project fixture. `pnpm sample:write` refreshes their
committed output; `pnpm sample:check` verifies it. The browser changes between
those recorded results and does not pretend to run Rust in the visitor's
browser.

GitHub Actions checks pull requests and deploys `main` to the existing Pages
site. The `CNAME` remains `ferralk.dev`.
