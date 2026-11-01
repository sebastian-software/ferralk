import type { ArdoMetaFunction } from "ardo/runtime";

import { ArdoLink as Link } from "ardo/runtime";
import { Mark, RegistryFacts } from "ferramenta-family";
import {
  FerraAction,
  FerraArticle,
  FerraChoices,
  FerraCode,
  FerraColumns,
  FerraEditorialRow,
  FerraFeatureGrid,
  FerraInstrument,
  FerraNotes,
  FerraProductLead,
  FerraProductPage,
  FerraReleaseFacts,
} from "ferramenta-family/ferra";
import { family, WORKSHOP } from "ferramenta-family/registry";

import { WalkExample } from "../components/walk-example";
import benchmarks from "../data/benchmarks.json";
import site from "../data/site.json";

// oxlint-disable-next-line react/only-export-components -- Ardo reads route metadata.
export const meta: ArdoMetaFunction = () => [
  { title: "Ferralk — Find files. Skip whole directories." },
  {
    name: "description",
    content:
      "Glob matching and parallel file discovery in Rust. Prune from your patterns, keep native filenames intact and choose traversal policy explicitly.",
  },
];
// oxlint-disable-next-line react/only-export-components -- Ardo reads the route layout.
export const handle = { layout: "bare" };

const member = family.find((entry) => entry.name === "ferralk");
if (!member) throw new Error("Ferralk is missing from the family registry.");
const tool = { ...member, version: site.version };
const ferralk = benchmarks.rows.find(({ name }) => name === "ferralk, 4 threads");
const ignore = benchmarks.rows.find(({ name }) => name === "ignore parallel + hand-pruned subtree");
if (!ferralk || !ignore) throw new Error("The published benchmark rows are missing.");
const timings = { ferralk: ferralk.scoped, ignore: ignore.scoped };

function Opening() {
  return (
    <FerraProductLead
      id="overview"
      title="Find files. Skip whole directories."
      introduction="Glob matching and parallel file discovery in Rust. Name the roots in your query and Ferralk can leave unrelated directories closed. Keep native filenames intact and choose exactly how your tool walks the tree."
      art={<FerraInstrument product="ferralk" />}
      byline={
        <>
          An open-source project by <a href={WORKSHOP.openSource}>{WORKSHOP.company}</a>.
        </>
      }
    >
      <div className="ferra-actions">
        <FerraAction href="/guide/getting-started/">Start with Rust</FerraAction>
        <FerraAction variant="secondary" href="#example">
          See a file search
        </FerraAction>
      </div>
    </FerraProductLead>
  );
}

function Highlights() {
  return (
    <FerraColumns>
      <FerraArticle
        title="The fastest directory is the one you never open."
        icon={<Mark name="package" size={28} />}
        meta={<FerraReleaseFacts tool={tool} />}
      >
        <p>
          A query such as <code>{"{src,packages}/**/*.{ts,tsx}"}</code> names its possible roots.
          Ferralk derives the pruning from that pattern, so finding source files does not mean
          opening <code>node_modules</code> first.
        </p>
        <Link to="/evidence/benchmarks/">See the measured difference</Link>
      </FerraArticle>
      <FerraArticle
        title="A filename should survive the search."
        icon={<Mark name="document" size={28} />}
        meta={<Link to="/reference/stability/">The 1.x stability contract</Link>}
      >
        <p>
          Paths remain native paths, including names that cannot be represented as ordinary UTF-8
          text. The matcher works on bytes and forbids unsafe Rust; traversal policy stays explicit.
        </p>
        <Link to="/reference/patterns/">Understand the path contract</Link>
      </FerraArticle>
    </FerraColumns>
  );
}

function Policy() {
  return (
    <FerraEditorialRow
      notes={<FerraNotes label="Inside Ferralk" notes={tool.editorial?.notes ?? []} />}
    >
      <FerraArticle
        title="One engine for the search. Your policy around it."
        introduction="Build tools, linters and indexers need a file list they can explain. Ferralk gives each traversal choice a documented default."
        meta={<Link to="/guide/usage/">Every default and the switch that changes it</Link>}
      >
        <FerraFeatureGrid
          items={[
            {
              heading: "Git ignores, when you ask",
              text: "Opt in to .gitignore and .ignore handling. Nested rules and Git configuration are checked against Git itself.",
            },
            {
              heading: "Parallel or incremental",
              text: "Collect with a worker budget, stream incrementally, or visit entries with your own filter and early-stop decision.",
            },
            {
              heading: "Errors alongside the result",
              text: "Recoverable errors are collected by default. Inspect them, or explicitly choose to skip or abort.",
            },
            {
              heading: "The pattern language you mean",
              text: "Ordinary wildcards stay within a path component. Recursive **, braces and extglobs are enabled by the walker, and opt-in in the matcher.",
            },
          ]}
        />
      </FerraArticle>
    </FerraEditorialRow>
  );
}

function Performance() {
  return (
    <FerraArticle
      title="Less walking shows up in the numbers."
      meta={<Link to="/evidence/benchmarks/">Full comparisons, commands and limitations</Link>}
    >
      <p>
        On the published repository-shaped fixture, the portable four-thread walk completes the
        scoped query in <strong>{timings.ferralk}</strong>. The <code>ignore</code> comparison with
        hand-written subtree pruning takes <strong>{timings.ignore}</strong>. Ferralk derives that
        saving from the include pattern itself.
      </p>
      <p>
        This is the {benchmarks.date} snapshot on an Apple M1 Pro with a warm cache and a fixed
        worker budget. It describes one tree shape, rather than every filesystem. The evidence also
        separates matcher work, broad queries, native backends and Linux measurements.
      </p>
      <Link to="/evidence/palamedes/">
        What changed when Palamedes tried it on real repositories
      </Link>
    </FerraArticle>
  );
}

function GettingStarted() {
  return (
    <FerraChoices>
      <FerraArticle
        id="start"
        title="Find files on disk."
        introduction="Use the walker crate for parallel traversal, include and exclude patterns, Git ignores and explicit error handling. It re-exports the matcher too."
      >
        <FerraCode caption="Add the walker">cargo add ferralk</FerraCode>
        <FerraAction href="/guide/getting-started/">Build your first search</FerraAction>
      </FerraArticle>
      <FerraArticle
        title="Already have the paths?"
        introduction="Use the matcher on its own. Compile a pattern once and match it against the paths or bytes you hold, without pulling in traversal dependencies."
      >
        <FerraCode caption="Add only the matcher">cargo add ferralk-glob</FerraCode>
        <FerraAction variant="secondary" href="/guide/usage/#match-paths-deliberately">
          Start with matching
        </FerraAction>
      </FerraArticle>
    </FerraChoices>
  );
}

function Closing() {
  return (
    <FerraArticle
      title="A familiar idea, with its decisions written down."
      meta={<a href={WORKSHOP.openSource}>More open source from {WORKSHOP.company}</a>}
    >
      <p>
        Ferralk learns from zlob’s byte-first matcher and walker semantics and checks them against a
        frozen reference. The implementation runs in Rust, with an independent API and no C ABI.
        Architecture records explain the choices; the behavioral corpus makes their consequences
        testable.
      </p>
      <p>
        <Link to="/guide/migration/">Coming from another library?</Link>
        {" · "}
        <Link to="/evidence/verification/">Explore the verification</Link>
        {" · "}
        <a href="https://ferramenta.dev">Meet the Ferramenta family</a>
      </p>
    </FerraArticle>
  );
}

export default function HomePage() {
  return (
    <RegistryFacts>
      <FerraProductPage>
        <Opening />
        <Highlights />
        <WalkExample />
        <Policy />
        <Performance />
        <GettingStarted />
        <Closing />
      </FerraProductPage>
    </RegistryFacts>
  );
}
