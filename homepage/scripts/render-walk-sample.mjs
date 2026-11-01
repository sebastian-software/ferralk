/* eslint-disable security/detect-non-literal-fs-filename -- The fixture is owned by this script in a fresh temporary directory. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { format } from "oxfmt";

const repository = fileURLToPath(new URL("../../", import.meta.url));
const output = new URL("../app/data/walk-sample.json", import.meta.url);
const fixture = await mkdtemp(join(tmpdir(), "ferralk-site-sample-"));
const files = [
  "src/main.ts",
  "src/components/button.tsx",
  "packages/ui/index.ts",
  "public/legacy.ts",
  "node_modules/vendor/index.ts",
  "dist/main.js",
  ".cache/old.ts",
];
try {
  await mkdir(join(fixture, ".git"));
  for (const file of files) {
    await mkdir(dirname(join(fixture, file)), { recursive: true });
    await writeFile(join(fixture, file), "// Fixture for the native Ferralk example.\n");
  }
  await writeFile(join(fixture, ".gitignore"), "node_modules/\ndist/\n");
  const queries = [
    {
      id: "scoped",
      label: "Source roots only",
      pattern: "{src,packages}/**/*.{ts,tsx}",
      expected: ["packages/ui/index.ts", "src/components/button.tsx", "src/main.ts"],
    },
    {
      id: "unscoped",
      label: "The whole project",
      pattern: "**/*.{ts,tsx}",
      expected: [
        "packages/ui/index.ts",
        "public/legacy.ts",
        "src/components/button.tsx",
        "src/main.ts",
      ],
    },
  ];
  const samples = queries.map(({ expected, ...query }) => {
    const stdout = execFileSync(
      "cargo",
      [
        "run",
        "--quiet",
        "--locked",
        "--manifest-path",
        join(repository, "Cargo.toml"),
        "--example",
        "find",
        "--",
        query.pattern,
      ],
      { cwd: fixture, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
    );
    const paths = stdout
      .trim()
      .split("\n")
      .map((path) => path.replace(/^\.\//, ""));
    assert.deepEqual(paths, expected, `${query.id}: the native example changed its result`);
    return { ...query, paths };
  });
  const result = await format(
    "walk-sample.json",
    JSON.stringify({ files, ignore: "node_modules/\ndist/", samples }),
  );
  assert.equal(result.errors.length, 0);
  if (process.argv.includes("--check"))
    assert.equal(
      await readFile(output, "utf8"),
      result.code,
      "Refresh the native walk sample with pnpm sample:write.",
    );
  else await writeFile(output, result.code);
  console.log("Verified both query results with Ferralk's actual Rust find example.");
} finally {
  await rm(fixture, { recursive: true, force: true });
}
