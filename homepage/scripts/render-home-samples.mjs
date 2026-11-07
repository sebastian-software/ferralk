/* eslint-disable security/detect-non-literal-fs-filename -- Only fixed, repository-owned paths are read or written. */
/**
 * Writes app/data/home-samples.json, everything the homepage shows:
 *
 * - the matcher, walk and byte-name specimens, printed by homepage/samples, a
 *   small program that depends on the published ferralk and ferralk-glob
 *   crates from crates.io at the exact versions pinned in
 *   homepage/samples/Cargo.toml;
 * - the speed comparison, read from the published tables in
 *   docs/benchmark-evidence.md.
 *
 * The corpus counts change with every new case, so write-site-data.mjs derives
 * them at build time instead.
 *
 * `pnpm sample:write` writes the file, `pnpm sample:check` fails when it no
 * longer matches. Nobody edits the file by hand.
 */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { format } from "oxfmt";

const root = fileURLToPath(new URL("../../", import.meta.url));
const check = process.argv.includes("--check");
const destination = new URL("../app/data/home-samples.json", import.meta.url);
const repository = (path) => new URL(`../../${path}`, import.meta.url);

// The specimens: real output of the pinned, published crates.
const manifest = await readFile(new URL("../samples/Cargo.toml", import.meta.url), "utf8");
const pinned = /^ferralk = "=([^"]+)"$/m.exec(manifest)?.[1];
const pinnedGlob = /^ferralk-glob = "=([^"]+)"$/m.exec(manifest)?.[1];
assert(pinned, "homepage/samples/Cargo.toml must pin ferralk to one exact version.");
assert.equal(pinnedGlob, pinned, "Pin ferralk and ferralk-glob to the same release.");
const releases = JSON.parse(await readFile(repository(".release-please-manifest.json"), "utf8"));
const release = releases["."];
if (!check && pinned !== release) {
  console.warn(
    `Rendering with ferralk ${pinned}; the current release is ${release}. Update the pins in homepage/samples/Cargo.toml once ${release} is on crates.io.`,
  );
}
const specimens = JSON.parse(
  execFileSync(
    "cargo",
    [
      "run",
      "--locked",
      "--quiet",
      "--manifest-path",
      "homepage/samples/Cargo.toml",
      "--target-dir",
      "target/homepage-samples",
    ],
    { cwd: root, encoding: "utf8" },
  ),
);
const { walk } = specimens;
const query = (id) => walk.queries.find((entry) => entry.id === id);
assert.equal(query("scoped")?.errors.length, 0, "The scoped walk should never open pgdata.");
assert.equal(query("unscoped")?.errors.length, 1, "The broad walk should report pgdata.");

// The speed comparison: the published M1 Pro tables, as times relative to
// Ferralk within the same table.
const evidence = await readFile(repository("docs/benchmark-evidence.md"), "utf8");
const expanded = evidence.split("## Expanded library comparison on this machine, ")[1];
assert(expanded, "The expanded library comparison is missing.");
const date = /^(\d{4}-\d{2}-\d{2})/.exec(expanded)?.[1];
const hostRow = (name) => {
  const prefix = `| ${name} | `;
  const row = expanded
    .split("\n")
    .find((line) => line.startsWith(prefix))
    ?.slice(prefix.length, -2);
  assert(row, `The benchmark host table has no ${name} row.`);
  return row;
};
const machine = /Apple M\d[^,]*/.exec(hostRow("Host"))?.[0];
const measuredVersion = /^(\S+),/.exec(hostRow("Ferralk version"))?.[1];
const revision = /`([0-9a-f]{7,})`/.exec(hostRow("Benchmark revision"))?.[1];
assert(date && machine && measuredVersion && revision, "The benchmark host table is incomplete.");
const fixture = /^([\d,]+) files, ([\d,]+) TypeScript sources/.exec(hostRow("Fixture"));
const threads = /^(\d+) for every/.exec(hostRow("Threads"))?.[1];
assert(fixture && threads, "The benchmark fixture or thread count is missing.");
const count = (text) => Number(text.replaceAll(",", ""));

/** A Markdown table under a heading, as rows of plain cells. */
function table(heading, text = expanded) {
  const section = text.split(`${heading}\n`)[1];
  assert(section, `The benchmark evidence has no "${heading}" section.`);
  const lines = section.split("\n");
  const start = lines.findIndex((line) => line.startsWith("|"));
  const end = lines.findIndex((line, index) => index > start && !line.startsWith("|"));
  return lines.slice(start + 2, end).map((line) =>
    line
      .split("|")
      .slice(1, -1)
      .map((cell) => cell.trim().replaceAll("*", "").replaceAll("`", "")),
  );
}
const milliseconds = (cell) => {
  const value = /^([\d.]+) ms$/.exec(cell)?.[1];
  assert(value, `Not a time in milliseconds: ${cell}`);
  return Number(value);
};
const nanoseconds = (cell) => {
  const value = /^([\d.]+) ns$/.exec(cell)?.[1];
  assert(value, `Not a time in nanoseconds: ${cell}`);
  return Number(value);
};
const portable = table("### Portable backend");
const arm = (name) => {
  const row = portable.find(([label]) => label === name);
  assert(row, `The portable table has no "${name}" row.`);
  return row;
};
const matcherRows = table("### Matcher refresh");
const matcherHeader = (() => {
  const section = expanded.split("### Matcher refresh\n")[1] ?? "";
  const line = section.split("\n").find((entry) => entry.startsWith("| Case"));
  assert(line, "The matcher table has no header.");
  return line
    .split("|")
    .slice(1, -1)
    .map((cell) => cell.trim().replaceAll("`", ""));
})();
const matcherCase = (name) => {
  const row = matcherRows.find(([label]) => label === name);
  assert(row, `The matcher table has no "${name}" row.`);
  return (tool) => nanoseconds(row[matcherHeader.indexOf(tool)]);
};

/** Each task: Ferralk's arm and up to three alternatives, as times relative to Ferralk. */
function task({ id, unit, own }, alternatives) {
  return {
    id,
    unit,
    ferralk: own,
    rows: alternatives.map(({ id: tool, label, value }) => ({
      id: tool,
      label,
      value,
      ratio: Number((value / own).toFixed(2)),
    })),
  };
}
const unscoped = (name) => milliseconds(arm(name)[1]);
const scoped = (name) => milliseconds(arm(name)[2]);
const common = matcherCase("common matching");
const matcherBench = await readFile(repository("tools/bench/benches/matcher.rs"), "utf8");
const commonCase = {
  pattern: /let common_pattern = "([^"]+)";/.exec(matcherBench)?.[1],
  path: /let common_matching = "([^"]+)";/.exec(matcherBench)?.[1],
};
assert(commonCase.pattern && commonCase.path, "The matcher benchmark's common case moved.");
const speed = {
  measured: {
    date,
    machine,
    ferralk: measuredVersion,
    revision,
    files: count(fixture[1]),
    sources: count(fixture[2]),
    threads: Number(threads),
    matcher: commonCase,
  },
  tasks: [
    task({ id: "roots", unit: "ms", own: scoped("ferralk, 4 threads") }, [
      {
        id: "ignore",
        label: "ignore",
        value: scoped("ignore parallel + hand-pruned subtree"),
      },
      { id: "zlob", label: "zlob", value: scoped("zlob, 4 threads") },
      { id: "jwalk", label: "jwalk + globset", value: scoped("jwalk, 4 threads + globset") },
    ]),
    task({ id: "tree", unit: "ms", own: unscoped("ferralk, 4 threads") }, [
      { id: "jwalk", label: "jwalk + globset", value: unscoped("jwalk, 4 threads + globset") },
      { id: "zlob", label: "zlob", value: unscoped("zlob, 4 threads") },
      {
        id: "ignore",
        label: "ignore",
        value: unscoped("ignore parallel + overrides"),
      },
    ]),
    task({ id: "match", unit: "ns", own: common("ferralk") }, [
      { id: "zlob", label: "zlob", value: common("zlob") },
      { id: "globset", label: "globset", value: common("globset") },
      { id: "fast-glob", label: "fast-glob", value: common("fast-glob") },
    ]),
  ],
};

const samples = {
  note: "Generated by homepage/scripts/render-home-samples.mjs. Do not edit.",
  renderedWith: { ferralk: pinned },
  ...specimens,
  speed,
};
const formatted = await format("home-samples.json", JSON.stringify(samples));
assert.equal(formatted.errors.length, 0);
if (check) {
  assert.equal(
    await readFile(destination, "utf8"),
    formatted.code,
    "The homepage samples changed. Run pnpm sample:write and commit the result.",
  );
  console.log(`The homepage samples match ferralk ${pinned} and the benchmarks.`);
} else {
  await writeFile(destination, formatted.code);
  console.log(`Wrote app/data/home-samples.json with ferralk ${pinned}.`);
}
