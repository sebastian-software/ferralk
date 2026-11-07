import type { ArdoMetaFunction } from "ardo/runtime";

import { Fragment } from "ardo/react";
import { ArdoLink as Link } from "ardo/runtime";
import {
  MonoAction,
  MonoClosing,
  MonoColophon,
  MonoEngraving,
  MonoOpening,
} from "ferramenta-family/monografie";
import { WORKSHOP } from "ferramenta-family/registry";

import { corpus, ferralk, lowerFirst } from "../components/home-data";
import { Fit, Policy, Speed, Verification } from "../components/home-engine";
import { Explainer, Names, Pruning } from "../components/home-reading";
import { rustVersion, version } from "../data/site.json";

// oxlint-disable-next-line react/only-export-components -- Ardo reads route metadata.
export const meta: ArdoMetaFunction = () => [
  { title: "Ferralk — Find files. Skip whole folders." },
  {
    name: "description",
    content:
      "A glob matcher and parallel file walker in Rust. Name the folders in your pattern and Ferralk leaves the rest closed, keeps native file names intact and makes every traversal choice explicit.",
  },
];
// oxlint-disable-next-line react/only-export-components -- Ardo reads the route layout.
export const handle = { layout: "bare" };

const releaseLine = version.split(".").slice(0, 2).join(".");

function Actions() {
  return (
    <>
      <MonoAction href="/guide/getting-started/" primary detail="cargo add ferralk">
        Start with Rust
      </MonoAction>
      <MonoAction href="/guide/usage/#match-paths-deliberately" detail="cargo add ferralk-glob">
        Match paths only
      </MonoAction>
    </>
  );
}

function Opening() {
  return (
    <MonoOpening
      product="ferralk"
      instrumentLabel="Ferralk's instrument: a steel rail with an engraved scale that forks into three branches, each ending at a tabbed file leaf. A rust-brown arm pivots from the fork toward one branch, and a dark block stops the lower branch halfway."
      caption="A path gate: the selector turns toward the branch the pattern names, and a stop closes another before the walk goes down it."
      kicker={`Ferralk ${releaseLine}, ${lowerFirst(ferralk.what)} for Rust`}
      title="Find files. Skip whole folders."
      subtitle="Name the folders, and the rest stays closed."
      standfirst={`Every build tool, linter and indexer starts by finding files: all TypeScript sources, every test, everything except the dependencies. You describe them with a glob pattern such as src/**/*.ts. Ferralk is a Rust library that matches those patterns and walks the folders on disk to find them, in parallel. It reads the pattern before it walks, so folders that cannot hold a match are never opened. ${ferralk.audience}`}
      actions={<Actions />}
      facts={[
        {
          value: <em>{corpus.cases.toLocaleString("en-US")}</em>,
          label: "recorded cases, each naming where its answer comes from",
        },
        { value: "Bytes", label: "file names matched as stored, never converted to text" },
        {
          value: (
            <>
              <em>2</em> crates
            </>
          ),
          label: "the walker, and the matcher on its own",
        },
        {
          value: `Rust ${rustVersion}+`,
          label: "MIT or Apache-2.0, no unsafe code in the matcher",
        },
      ]}
    />
  );
}

// Keys are written out: minified builds do not keep function names.
const chapters = [
  { key: "explainer", Chapter: Explainer },
  { key: "pruning", Chapter: Pruning },
  { key: "names", Chapter: Names },
  { key: "policy", Chapter: Policy },
  { key: "speed", Chapter: Speed },
  { key: "verification", Chapter: Verification },
  { key: "fit", Chapter: Fit },
];

/** Where Ferralk comes from, and who makes it. */
function Origin() {
  return (
    <section aria-label="Origin">
      <div className="mono-axis">
        <MonoColophon>
          {ferralk.proof} It is one of the Ferramenta engines, made by {WORKSHOP.name} in{" "}
          {WORKSHOP.place}.
        </MonoColophon>
      </div>
    </section>
  );
}

export default function HomePage() {
  return (
    <>
      <Opening />
      {chapters.map(({ key, Chapter }, index) => (
        <Fragment key={key}>
          <MonoEngraving seed={index + 1} />
          <Chapter />
        </Fragment>
      ))}
      <MonoEngraving seed={chapters.length + 1} />
      <Origin />
      <div id="start">
        <MonoClosing
          title="Start with the file list."
          standfirst="Add the walker to search the disk, or only the matcher for paths you already hold. Both read the glob patterns your users already write."
          actions={<Actions />}
          links={
            <>
              <a href="https://github.com/sebastian-software/ferralk">GitHub repository</a>
              <Link to="/guide/getting-started/">Documentation</Link>
              <a href="https://docs.rs/ferralk">API reference</a>
              <a href={WORKSHOP.consulting}>Integration and support</a>
            </>
          }
        />
      </div>
    </>
  );
}
