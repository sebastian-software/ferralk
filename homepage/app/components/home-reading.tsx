import { ArdoLink as Link } from "ardo/runtime";
import { MonoCredit, MonoLeadIn, MonoPassage, MonoSection } from "ferramenta-family/monografie";

import { registryNote } from "./home-data";
import { BytesSpecimen, MatcherSpecimen, WalkSpecimen } from "./walk-specimens";

/* The homepage's reading chapters: what a glob is, pruning, byte names. */

export function Explainer() {
  return (
    <MonoSection id="overview" title="One line that describes a set of files.">
      <MonoPassage
        note={{
          term: "Glob pattern",
          children:
            "A short text with wildcards, like src/**/*.ts, that stands for many file paths at once. Shells, build tools and .gitignore files all use them.",
        }}
      >
        <p>
          <MonoLeadIn>Before a build tool</MonoLeadIn> compiles anything, before a linter checks a
          single line, it has to know which files to look at. Nobody lists them one by one. You
          write a pattern instead: <code>src/**/*.ts</code> means every TypeScript file anywhere
          under <code>src</code>. Such patterns are called globs, and you have typed them in a
          terminal, in a <code>.gitignore</code> and in the settings of nearly every tool you use.
        </p>
      </MonoPassage>
      <MonoPassage
        note={{
          side: "right",
          term: "Path component",
          children:
            "One folder or file name between two slashes. The path src/components/button.tsx has three.",
        }}
      >
        <p>
          Globs look simple and hide a few decisions. Does a star reach into subfolders? Does it
          match a name that starts with a period? Ferralk answers the way a shell does: a single
          star stays inside one folder, the double star is the explicit way to go deeper, and a
          wildcard leaves a leading period alone unless you ask. Switch between the patterns below
          to see which of the same five paths each one selects.
        </p>
      </MonoPassage>
      <MatcherSpecimen />
      <MonoCredit>
        <Link to="/guide/usage/#match-paths-deliberately">
          Match paths you already hold with ferralk-glob
        </Link>
      </MonoCredit>
    </MonoSection>
  );
}

export function Pruning() {
  return (
    <MonoSection
      id="example"
      title="Let the pattern narrow the search."
      standfirst="Most file walkers open every folder and filter what they find. Ferralk reads the pattern first and leaves the folders closed that cannot hold a match."
    >
      <MonoPassage
        note={{
          term: "Walking a file tree",
          children:
            "Listing a folder, then every folder inside it, and so on, until the whole tree has been read.",
        }}
      >
        <p>
          <MonoLeadIn>A JavaScript project</MonoLeadIn> may hold a few thousand source files beside
          a <code>node_modules</code> folder with tens of thousands more. A walker that lists
          everything and filters afterwards pays for every one of those folders, even when the
          question was only about <code>src</code>. The pattern{" "}
          <code>{"{src,packages}/**/*.ts"}</code> already says where the answer can be: under{" "}
          <code>src</code> or under <code>packages</code>, and nowhere else.
        </p>
      </MonoPassage>
      <MonoPassage
        note={{
          side: "right",
          term: "Ignore rules",
          children:
            "The lines of a .gitignore file: names and patterns that Git leaves out. Ferralk follows them when you ask it to.",
        }}
      >
        <p>
          Ferralk takes the folder names a pattern fixes and starts its walk there. Ignore rules and
          excludes close more folders on the way, so an ignored <code>node_modules</code> stays shut
          whatever the pattern says. Choose a search below and compare what it finds with what it
          runs into.
        </p>
      </MonoPassage>
      <WalkSpecimen />
      <MonoCredit>
        <Link to="/guide/usage/">Matching, defaults and error handling</Link>
      </MonoCredit>
    </MonoSection>
  );
}

export function Names() {
  const note = registryNote("A file name is not always a string");
  return (
    <MonoSection
      id="names"
      title="A file name is a row of bytes."
      standfirst="Most names are valid text. The ones that are not still belong to real files, and Ferralk keeps them intact."
    >
      <MonoPassage
        note={{
          term: "Encoding",
          children:
            "The rule that turns letters into bytes. UTF-8 is the usual one today, but Linux also accepts file names that are not valid UTF-8, and Windows names that are not valid UTF-16.",
        }}
      >
        <p>
          <MonoLeadIn>To your file system</MonoLeadIn>, a name is a sequence of bytes. Most of them
          spell valid UTF-8 text, but old archives, mounted disks and other systems bring names in
          older encodings such as Latin-1. A program that turns such a name into text has to replace
          what it cannot read, and the replacement character looks the same for every unreadable
          byte.
        </p>
      </MonoPassage>
      <MonoPassage note={{ side: "right", term: note.title, children: note.body }}>
        <p>
          Ferralk matches the bytes themselves, on every platform: the raw bytes on Unix and a
          lossless encoding of the name on Windows. A path the walker returns is a path you can open
          again, and two different files keep two different names.
        </p>
      </MonoPassage>
      <BytesSpecimen />
      <MonoCredit>
        <Link to="/reference/patterns/">The path contract</Link>
      </MonoCredit>
    </MonoSection>
  );
}
