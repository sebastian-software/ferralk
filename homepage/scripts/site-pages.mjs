/* eslint-disable security/detect-non-literal-fs-filename -- Documentation discovery is limited to the repository's docs directory. */
import { readdirSync, readFileSync } from "node:fs";

const featured = new Map([
  ["homepage/content/getting-started.md", "guide/getting-started"],
  ["docs/usage.md", "guide/usage"],
  ["docs/compatibility-guide.md", "guide/migration"],
  ["docs/consumer-contract.md", "reference/patterns"],
  ["docs/stability.md", "reference/stability"],
  ["docs/compatibility-matrix.md", "reference/compatibility"],
  ["docs/fast-glob-reference.md", "reference/fast-glob"],
  ["docs/benchmark-evidence.md", "evidence/benchmarks"],
  ["docs/palamedes-adoption.md", "evidence/palamedes"],
  ["docs/verification-comparison.md", "evidence/verification"],
  ["docs/corpus-format.md", "contributing/corpus"],
  ["CONTRIBUTING.md", "contributing/development"],
  ["fuzz/README.md", "contributing/fuzzing"],
  ["README.md.src", "reference/readme"],
  ["RFC-zig-free-zlob-port.md", "architecture/design"],
  ["docs/adr/README.md", "architecture/decisions"],
]);
const docs = readdirSync(new URL("../../docs/", import.meta.url), { recursive: true })
  .map((file) => file.replaceAll("\\", "/"))
  .filter((file) => file.endsWith(".md"))
  .map((file) => `docs/${file}`);

export const documents = [...new Set([...featured.keys(), ...docs])].map((source) => {
  const path =
    featured.get(source) ??
    (source.startsWith("docs/adr/")
      ? `architecture/adr/${source.slice("docs/adr/".length, -3)}`
      : `archive/${source.slice("docs/".length, -3).replace(/README$/, "index")}`.replace(
          /\/index$/,
          "",
        ));
  const content = readFileSync(new URL(`../../${source}`, import.meta.url), "utf8");
  const frontmatter = content.startsWith("---\n") ? content.split("---\n")[1] : undefined;
  const title =
    (frontmatter && /^title: (.+)$/m.exec(frontmatter)?.[1]) ??
    /^# (.+)$/m.exec(content)?.[1].replaceAll("`", "") ??
    source;
  return { source, path, title, section: path.split("/")[0] };
});

export const expectedPages = [
  "index.html",
  ...documents.map(({ path }) => `${path}/index.html`),
].sort();

export function pagePath(page) {
  return page === "index.html" ? "/" : `/${page.replace(/index\.html$/, "")}`;
}
