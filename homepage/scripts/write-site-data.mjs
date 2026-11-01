/* eslint-disable security/detect-non-literal-fs-filename -- Source and output paths come from the repository-owned document manifest. */
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { posix } from "node:path";
import { format } from "oxfmt";

import { documents } from "./site-pages.mjs";

const repository = new URL("../../", import.meta.url);
const routes = new URL("../app/routes/", import.meta.url);
const data = new URL("../app/data/", import.meta.url);
const destinations = new Map(documents.map(({ source, path }) => [source, `/${path}/`]));
destinations.set("README.md", "/reference/readme/");
const repoUrl = "https://github.com/sebastian-software/ferralk";
await mkdir(data, { recursive: true });
for (const section of ["reference", "evidence", "architecture", "contributing", "archive"]) {
  await rm(new URL(`${section}/`, routes), { recursive: true, force: true });
}

for (const document of documents) {
  const source = await readFile(new URL(document.source, repository), "utf8");
  const body = source.startsWith("---\n") ? source.slice(source.indexOf("\n---\n") + 5) : source;
  const content = rewriteLinks(body.replace(/^# .+\n/, ""), document.source);
  const destination = new URL(`${document.path}.md`, routes);
  await mkdir(new URL(".", destination), { recursive: true });
  await writeFile(destination, `---\ntitle: ${JSON.stringify(document.title)}\n---\n${content}`);
}

const cargo = await readFile(new URL("Cargo.toml", repository), "utf8");
const version = /^version = "([^"]+)"/m.exec(cargo)?.[1];
if (!version) throw new Error("The package has no release version.");
await writeData("site.json", { version });
const labels = {
  guide: "Guide",
  reference: "Reference",
  evidence: "Evidence",
  architecture: "Architecture",
  contributing: "Contributing",
  archive: "Archive",
};
await writeData(
  "navigation.json",
  Object.entries(labels).map(([id, label]) => {
    const pages = documents
      .filter((document) => document.section === id)
      .map(({ path, title }) => ({ title, to: `/${path}/` }));
    return { id, label, to: pages[0].to, pages };
  }),
);
const evidence = await readFile(new URL("docs/benchmark-evidence.md", repository), "utf8");
const portable = evidence.split("### Portable backend\n")[1]?.split("### macOS-native backend")[0];
if (!portable) throw new Error("The published portable benchmark table is missing.");
const rows = portable
  .split("\n")
  .filter((line) => line.startsWith("| ") && !line.startsWith("| ---") && !line.startsWith("| Arm"))
  .map((line) => {
    const [name, unscoped, scoped] = line
      .split("|")
      .slice(1, -1)
      .map((cell) => cell.trim().replaceAll("*", "").replaceAll("`", ""));
    return { name, unscoped, scoped };
  });
const date = /## Expanded library comparison on this machine, (\d{4}-\d{2}-\d{2})/.exec(
  evidence,
)?.[1];
if (!date || !rows.some((row) => row.name === "ferralk, 4 threads"))
  throw new Error("Benchmark metadata is missing.");
await writeData("benchmarks.json", { date, rows });

async function writeData(name, value) {
  const result = await format(name, JSON.stringify(value));
  if (result.errors.length) throw new Error(`Cannot format ${name}`);
  await writeFile(new URL(name, data), result.code);
}

function rewriteLinks(markdown, source) {
  let fence;
  return markdown
    .split("\n")
    .map((line) => {
      const delimiter = /^\s*(`{3,}|~{3,})/.exec(line);
      if (delimiter) {
        if (!fence)
          fence = {
            character: delimiter[1][0],
            length: delimiter[1].length,
            rust: line.slice(delimiter[0].length).trim() === "rust",
          };
        else if (delimiter[1][0] === fence.character && delimiter[1].length >= fence.length)
          fence = undefined;
        return line;
      }
      if (fence) return fence.rust && /^#(?: |$)/.test(line) ? null : line;
      return line.replaceAll(/(?<=\]\()[^\s)]+/g, (href) => resolveLink(href, source));
    })
    .filter((line) => line !== null)
    .join("\n");
}

function resolveLink(href, source) {
  if (/^(?:[a-z][a-z0-9+.-]*:|\/|#)/i.test(href)) return href;
  const [file, fragment] = href.split("#");
  const target = posix.normalize(posix.join(posix.dirname(source), file));
  const destination = destinations.get(target);
  if (destination) {
    const aliases = {
      "patterns-are-written-with--on-every-platform": "patterns-are-written-with-on-every-platform",
      "contract-change-audit-since-090": "contract-change-audit-since-0-9-0",
      "10-release-checklist": "1-0-release-checklist",
      "releasing-10": "releasing-1-0",
    };
    return `${destination}${fragment ? `#${aliases[fragment] ?? fragment}` : ""}`;
  }
  const kind = file.endsWith("/") || !posix.extname(file) ? "tree" : "blob";
  return `${repoUrl}/${kind}/main/${target}${fragment ? `#${fragment}` : ""}`;
}
