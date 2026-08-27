#!/usr/bin/env node

import { globSync as nodeGlobSync } from "node:fs";
import {
  glob as nodeGlob,
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { performance } from "node:perf_hooks";

import fastGlob from "fast-glob";
import { fdir } from "fdir";
import { glob as globAsync, globSync } from "glob";
import micromatch from "micromatch";
import { Minimatch } from "minimatch";
import picomatch from "picomatch";
import { glob as tinyGlob, globSync as tinyGlobSync } from "tinyglobby";

const WALKER_WARMUPS = 2;
const WALKER_SAMPLES = 10;
const MATCHER_WARMUPS = 5;
const MATCHER_SAMPLES = 15;
const MATCHER_ITERATIONS = 100_000;
const ADVERSARIAL_MATCHER_ITERATIONS = 100;
const EXPECTED_FILES = 53_600;
const EXPECTED_SOURCES = 2_600;
const MATCHER_CASES = [
  {
    name: "common matching",
    pattern: "src/**/*.rs",
    candidate: "src/deep/nested/main.rs",
    expected: true,
    iterations: MATCHER_ITERATIONS,
  },
  {
    name: "common non-matching",
    pattern: "src/**/*.rs",
    candidate: "src/deep/nested/main.txt",
    expected: false,
    iterations: MATCHER_ITERATIONS,
  },
  {
    name: "long-path matching",
    pattern: "src/**/*.rs",
    candidate: longPath("main.rs"),
    expected: true,
    iterations: MATCHER_ITERATIONS,
  },
  {
    name: "long-path non-matching",
    pattern: "src/**/*.rs",
    candidate: longPath("main.txt"),
    expected: false,
    iterations: MATCHER_ITERATIONS,
  },
  {
    name: "backtracking non-matching",
    pattern: "a*a*a*a*b",
    candidate: "a".repeat(64),
    expected: false,
    iterations: ADVERSARIAL_MATCHER_ITERATIONS,
  },
];

const validateOnly = process.argv.includes("--validate-only");
const root = await mkdtemp(join(tmpdir(), "ferralk-node-bench-"));

try {
  const fixture = await createFixture(root);
  assertEqual(fixture.files, EXPECTED_FILES, "fixture file count");
  assertEqual(fixture.sources, EXPECTED_SOURCES, "fixture source count");

  const walkerResults = await benchmarkWalkers(root, validateOnly);
  validateMatchers();
  if (!validateOnly) {
    printWalkerResults(walkerResults);
    printMatcherResults(benchmarkMatchers());
  }
} finally {
  await rm(root, { recursive: true, force: true });
}

async function createFixture(fixtureRoot) {
  let files = 0;
  let sources = 0;

  for (let area = 0; area < 40; area += 1) {
    for (let module = 0; module < 10; module += 1) {
      const directory = join(
        fixtureRoot,
        "src",
        `area-${area}`,
        `module-${module}`,
      );
      await mkdir(directory, { recursive: true });
      for (let index = 0; index < 10; index += 1) {
        let name;
        let isSource;
        switch (index % 5) {
          case 0:
            name = `view-${index}.tsx`;
            isSource = true;
            break;
          case 1:
          case 2:
            name = `unit-${index}.ts`;
            isSource = true;
            break;
          case 3:
            name = `style-${index}.css`;
            isSource = false;
            break;
          default:
            name = `legacy-${index}.js`;
            isSource = false;
        }
        await writeFixtureFile(join(directory, name));
        files += 1;
        sources += Number(isSource);
      }
    }
  }

  for (let packageIndex = 0; packageIndex < 20; packageIndex += 1) {
    const directory = join(
      fixtureRoot,
      "packages",
      `pkg-${packageIndex}`,
      "src",
    );
    await mkdir(directory, { recursive: true });
    for (let index = 0; index < 20; index += 1) {
      const isSource = index % 2 === 0;
      const name = isSource ? `index-${index}.ts` : `index-${index}.js`;
      await writeFixtureFile(join(directory, name));
      files += 1;
      sources += Number(isSource);
    }
  }

  for (let packageIndex = 0; packageIndex < 400; packageIndex += 1) {
    const packageRoot = join(
      fixtureRoot,
      "node_modules",
      `dep-${packageIndex}`,
    );
    files += await writePackage(packageRoot, 100);
    if (packageIndex % 10 === 0) {
      for (let nested = 0; nested < 5; nested += 1) {
        files += await writePackage(
          join(packageRoot, "node_modules", `nested-${nested}`),
          40,
        );
      }
    }
  }

  return { files, sources };
}

async function writePackage(packageRoot, fileCount) {
  const directory = join(packageRoot, "lib");
  await mkdir(directory, { recursive: true });
  await writeFixtureFile(join(packageRoot, "package.json"));
  await writeFixtureFile(join(packageRoot, "README.md"));
  for (let index = 0; index < fileCount; index += 1) {
    let name;
    switch (index % 10) {
      case 0:
        name = `types-${index}.d.ts`;
        break;
      case 1:
        name = `meta-${index}.json`;
        break;
      default:
        name = `chunk-${index}.js`;
    }
    await writeFixtureFile(join(directory, name));
  }
  return fileCount + 2;
}

async function writeFixtureFile(path) {
  await writeFile(path, "fixture");
}

async function benchmarkWalkers(fixtureRoot, onlyValidate) {
  const queries = [
    { name: "unscoped", pattern: "**/*.{ts,tsx}", expected: 7_400 },
    {
      name: "scoped",
      pattern: "{src,packages}/**/*.{ts,tsx}",
      expected: 2_600,
    },
  ];
  const results = [];

  for (const query of queries) {
    const candidates = walkerCandidates(fixtureRoot, query.pattern);
    for (const candidate of candidates) {
      const found = await candidate.run();
      assertEqual(found, query.expected, `${query.name}/${candidate.name}`);
    }
    if (onlyValidate) {
      continue;
    }

    for (let warmup = 0; warmup < WALKER_WARMUPS; warmup += 1) {
      for (const candidate of rotate(candidates, warmup)) {
        await candidate.run();
      }
    }

    const samples = new Map(candidates.map(({ name }) => [name, []]));
    for (let round = 0; round < WALKER_SAMPLES; round += 1) {
      for (const candidate of rotate(candidates, round)) {
        const started = performance.now();
        const found = await candidate.run();
        const elapsed = performance.now() - started;
        assertEqual(found, query.expected, `${query.name}/${candidate.name}`);
        samples.get(candidate.name).push(elapsed);
      }
    }

    for (const candidate of candidates) {
      results.push({
        query: query.name,
        candidate: candidate.name,
        ...summarize(samples.get(candidate.name)),
      });
    }
  }

  return results;
}

function walkerCandidates(fixtureRoot, pattern) {
  const options = { cwd: fixtureRoot, dot: true, follow: false, nodir: true };
  const fastGlobOptions = {
    cwd: fixtureRoot,
    dot: true,
    followSymbolicLinks: false,
    onlyFiles: true,
  };
  const tinyOptions = {
    cwd: fixtureRoot,
    dot: true,
    followSymbolicLinks: false,
    onlyFiles: true,
  };

  return [
    {
      name: "node:fs sync",
      run: () => nodeGlobSync(pattern, { cwd: fixtureRoot }).length,
    },
    {
      name: "node:fs async",
      run: async () => {
        let count = 0;
        for await (const _entry of nodeGlob(pattern, { cwd: fixtureRoot })) {
          count += 1;
        }
        return count;
      },
    },
    { name: "glob sync", run: () => globSync(pattern, options).length },
    {
      name: "glob async",
      run: async () => (await globAsync(pattern, options)).length,
    },
    {
      name: "fast-glob sync",
      run: () => fastGlob.sync(pattern, fastGlobOptions).length,
    },
    {
      name: "fast-glob async",
      run: async () => (await fastGlob(pattern, fastGlobOptions)).length,
    },
    {
      name: "tinyglobby sync",
      run: () => tinyGlobSync(pattern, tinyOptions).length,
    },
    {
      name: "tinyglobby async",
      run: async () => (await tinyGlob(pattern, tinyOptions)).length,
    },
    {
      name: "fdir + picomatch sync",
      run: () => fdirPicomatchSync(fixtureRoot, pattern),
    },
    {
      name: "fdir + picomatch async",
      run: () => fdirPicomatchAsync(fixtureRoot, pattern),
    },
  ];
}

function fdirPicomatchSync(fixtureRoot, pattern) {
  const matcher = picomatch(pattern, { dot: true });
  return new fdir()
    .withRelativePaths()
    .filter(matcher)
    .crawl(fixtureRoot)
    .sync().length;
}

async function fdirPicomatchAsync(fixtureRoot, pattern) {
  const matcher = picomatch(pattern, { dot: true });
  return (
    await new fdir()
      .withRelativePaths()
      .filter(matcher)
      .crawl(fixtureRoot)
      .withPromise()
  ).length;
}

function benchmarkMatchers() {
  const results = [];

  for (const benchmarkCase of MATCHER_CASES) {
    const candidates = matcherCandidates(benchmarkCase.pattern);

    for (let warmup = 0; warmup < MATCHER_WARMUPS; warmup += 1) {
      for (const candidate of rotate(candidates, warmup)) {
        runMatcherSample(
          candidate.match,
          benchmarkCase.candidate,
          benchmarkCase.iterations,
        );
      }
    }

    const samples = new Map(candidates.map(({ name }) => [name, []]));
    for (let round = 0; round < MATCHER_SAMPLES; round += 1) {
      for (const candidate of rotate(candidates, round)) {
        const elapsed = runMatcherSample(
          candidate.match,
          benchmarkCase.candidate,
          benchmarkCase.iterations,
        );
        samples.get(candidate.name).push(
          (elapsed * 1_000_000) / benchmarkCase.iterations,
        );
      }
    }

    for (const candidate of candidates) {
      results.push({
        case: benchmarkCase.name,
        candidate: candidate.name,
        ...summarize(samples.get(candidate.name)),
      });
    }
  }

  return results;
}

function validateMatchers() {
  for (const benchmarkCase of MATCHER_CASES) {
    for (const candidate of matcherCandidates(benchmarkCase.pattern)) {
      assertEqual(
        candidate.match(benchmarkCase.candidate),
        benchmarkCase.expected,
        `${benchmarkCase.name}/${candidate.name}`,
      );
    }
  }
}

function matcherCandidates(pattern) {
  const minimatch = new Minimatch(pattern, { dot: true });
  return [
    { name: "picomatch", match: picomatch(pattern, { dot: true }) },
    { name: "micromatch", match: micromatch.matcher(pattern, { dot: true }) },
    { name: "minimatch", match: (candidate) => minimatch.match(candidate) },
  ];
}

function runMatcherSample(match, candidate, iterations) {
  let matches = 0;
  const started = performance.now();
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    matches += Number(match(candidate));
  }
  const elapsed = performance.now() - started;
  globalThis.__ferralkBenchmarkSink = matches;
  return elapsed;
}

function longPath(file) {
  let path = "src";
  for (let segment = 0; segment < 12; segment += 1) {
    path += `/segment${segment}`;
  }
  return `${path}/${file}`;
}

function rotate(values, offset) {
  const split = offset % values.length;
  return values.slice(split).concat(values.slice(0, split));
}

function summarize(values) {
  const sorted = [...values].sort((left, right) => left - right);
  return {
    median: percentile(sorted, 0.5),
    q1: percentile(sorted, 0.25),
    q3: percentile(sorted, 0.75),
  };
}

function percentile(sorted, fraction) {
  const index = (sorted.length - 1) * fraction;
  const lower = Math.floor(index);
  const upper = Math.ceil(index);
  if (lower === upper) {
    return sorted[lower];
  }
  return sorted[lower] + (sorted[upper] - sorted[lower]) * (index - lower);
}

function printWalkerResults(results) {
  console.log("\nNode.js walker comparison (milliseconds, median [Q1, Q3])");
  console.log("query\tcandidate\tmedian\tq1\tq3");
  for (const result of results) {
    console.log(
      `${result.query}\t${result.candidate}\t${result.median.toFixed(2)}\t${result.q1.toFixed(2)}\t${result.q3.toFixed(2)}`,
    );
  }
}

function printMatcherResults(results) {
  console.log("\nNode.js compiled matcher comparison (nanoseconds per match, median [Q1, Q3])");
  console.log("case\tcandidate\tmedian\tq1\tq3");
  for (const result of results) {
    console.log(
      `${result.case}\t${result.candidate}\t${result.median.toFixed(1)}\t${result.q1.toFixed(1)}\t${result.q3.toFixed(1)}`,
    );
  }
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    throw new Error(`${label}: expected ${expected}, got ${actual}`);
  }
}
