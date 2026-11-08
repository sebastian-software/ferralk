import { ArdoLink as Link } from "ardo/runtime";
import {
  MonoCredit,
  MonoPassage,
  MonoProofs,
  MonoSection,
  MonoTerms,
  MonoWide,
} from "ferramenta-family/monografie";

import {
  corpus,
  ferralk,
  lowerFirst,
  palamedes,
  registryNote,
  samples,
  spelled,
} from "./home-data";
import { SpeedComparison } from "./speed-comparison";

/* The homepage's engine chapters: policy, speed, verification and fit. */

const { speed } = samples;
const { measured } = speed;
const measuredOn = new Date(`${measured.date}T00:00:00Z`).toLocaleDateString("en-US", {
  year: "numeric",
  month: "long",
  day: "numeric",
  timeZone: "UTC",
});

export function Policy() {
  return (
    <MonoSection
      id="policy"
      title="One engine for the search. Your policy around it."
      standfirst="Build tools, linters and indexers need a file list they can explain. Every choice the walker makes has a documented default and a switch that changes it."
    >
      <MonoWide>
        <MonoTerms
          prose
          columns={2}
          items={[
            {
              term: "Git ignores, when you ask",
              description:
                "Opt in to .gitignore and .ignore files. Nested rules and Git’s own settings are checked against Git itself.",
            },
            {
              term: "Parallel or one by one",
              description:
                "Collect with a thread budget, take entries as they arrive, or decide about each entry with your own function while the walk runs in parallel.",
            },
            {
              term: "Errors next to the result",
              description:
                "A folder that cannot be read is reported beside the files, as in the search above. You can skip such errors instead, or stop at the first one.",
            },
            {
              term: "The pattern language you mean",
              description:
                "Wildcards stay inside one folder, as in a shell. The double star, braces and extglob groups are on in the walker and opt-in in the matcher.",
            },
          ]}
        />
      </MonoWide>
      <MonoCredit>
        <Link to="/guide/usage/">Every default and the switch that changes it</Link>
      </MonoCredit>
    </MonoSection>
  );
}

/** How the speed figures were measured, from the published evidence. */
function Conditions() {
  return (
    <>
      <h3>How this was measured</h3>
      <p>
        Recorded on {measuredOn} on an {measured.machine}, with Ferralk {measured.ferralk} at
        revision {measured.revision}: the latest published run. The current release has not been
        measured with this profile yet.
      </p>
      <p>
        A generated project of {measured.files.toLocaleString("en-US")} files,{" "}
        {measured.sources.toLocaleString("en-US")} of them TypeScript sources under src and
        packages. Every parallel walker used {spelled(measured.threads)} threads on a warm cache,
        and each had to find the same number of files before it was timed.
      </p>
      <p>
        The matcher figure checks <code>{measured.matcher.path}</code> against{" "}
        <code>{measured.matcher.pattern}</code>, compiled in advance. With the macOS-native
        directory reader, Ferralk and zlob come out level on the search through every folder.
      </p>
    </>
  );
}

export function Speed() {
  return (
    <MonoSection
      id="speed"
      title="Less walking shows up in the time."
      standfirst="Where the pattern names its folders, Ferralk finished well ahead of ignore, zlob and jwalk in the published run. Where every folder has to be read, it still came first, by a smaller margin."
    >
      <MonoPassage
        note={{
          figure: measured.files.toLocaleString("en-US"),
          children: "files in the generated project every walker searched",
        }}
      >
        <p>
          Finding files comes first in almost every build, lint and test run, and a tool that
          watches for changes repeats it again and again. Its cost grows with everything in the
          project, including the folders nobody asked about.
        </p>
      </MonoPassage>
      <p>
        The chart compares the same searches over the same project: a few thousand sources beside a
        large <code>node_modules</code>. Switch between a search that names its folders, a search
        through every folder, and matching a single path.
      </p>
      <MonoWide>
        <SpeedComparison
          after={
            <p>
              The published comparison also covers walkdir, globwalk, wax, Linux and the
              macOS-native backend.{" "}
              <Link to="/evidence/benchmarks/">Every arm, its command and its limits</Link>
            </p>
          }
          conditions={<Conditions />}
        />
      </MonoWide>
    </MonoSection>
  );
}

export function Verification() {
  const note = registryNote("Keep the reference, replace the runtime");
  return (
    <MonoSection
      id="verification"
      title="Held to a frozen reference."
      standfirst="Glob matching is full of small decisions. Ferralk writes each one down as a case, together with the tool its answer comes from."
    >
      <MonoPassage note={{ term: note.title, children: note.body }}>
        <p>
          Ferralk learns from zlob, a fast glob library written in Zig. Its matching and walking
          behavior was recorded from zlob {corpus.zlobVersion} and frozen, so a new zlob release can
          never move the expectations. The implementation is independent: plain Rust with its own
          API and no C interface.
        </p>
      </MonoPassage>
      <p>
        Every behavior the project promises lives in a checked-in collection of{" "}
        {corpus.cases.toLocaleString("en-US")} cases in {corpus.topics} topics, and each case names
        where its expected answer comes from. {corpus.decided} of them record a deliberate
        difference from the reference, each with the decision record that explains it. CI replays
        every case on every pull request, and a pinned Git replays the ignore cases.
      </p>
      <MonoProofs
        className="fk-proofs"
        items={[
          { label: `Recorded from zlob ${corpus.zlobVersion}`, value: corpus.zlob },
          { label: "Checked by Git’s own check-ignore", value: corpus.git },
          { label: "Shared with fast-glob, where the syntax overlaps", value: corpus.fastGlob },
          { label: "Written by hand for a policy or a past bug", value: corpus.handwritten },
        ]}
      />
      <MonoCredit>
        Counted from the corpus folder of the repository.{" "}
        <Link to="/evidence/verification/">How the verification works</Link>
      </MonoCredit>
    </MonoSection>
  );
}

/** Where Ferralk fits, and what it leaves out. */
const FIT_TERMS = [
  {
    term: "Search the disk",
    description: (
      <>
        Add the walker with <code>cargo add ferralk</code>. It includes the matcher.
      </>
    ),
  },
  {
    term: "Match paths you already have",
    description: (
      <>
        Add only the matcher with <code>cargo add ferralk-glob</code>, without the walker’s
        dependencies.
      </>
    ),
  },
  {
    term: "TypeScript applications",
    description: (
      <>
        <a href={palamedes.docs}>Palamedes</a>, {lowerFirst(palamedes.what)}, runs on Ferralk. There
        is no npm package of Ferralk itself.
      </>
    ),
  },
  {
    term: "Not included",
    description:
      "Reading the files it finds, watching them for changes, or a command-line tool. Ferralk hands your program the list.",
  },
];

export function Fit() {
  return (
    <MonoSection id="fit" title="A focused engine. Your tool around it.">
      <p>
        Ferralk is {lowerFirst(ferralk.what)}. It {lowerFirst(ferralk.does)} It finds the files and
        leaves the work on them to your program.
      </p>
      <MonoWide>
        <MonoTerms prose items={FIT_TERMS} />
      </MonoWide>
      <MonoCredit>
        <Link to="/guide/migration/">Coming from zlob?</Link>
      </MonoCredit>
    </MonoSection>
  );
}
