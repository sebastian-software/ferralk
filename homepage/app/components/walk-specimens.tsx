import { useState } from "ardo/react";
import {
  MonoCaption,
  MonoFacing,
  MonoSegmented,
  MonoSheet,
  MonoWide,
} from "ferramenta-family/monografie";

import { renderedWith, samples, spelled } from "./home-data";
import "./walk-specimens.css";

/* The homepage's specimens: every path, result and error here is real output
   of the published crates, from app/data/home-samples.json. */

const { matcher, walk, bytes } = samples;
const capitalized = (text: string) => `${text.charAt(0).toUpperCase()}${text.slice(1)}`;

/** Numbered lines of code or of a file; a long line wraps under a hanging indent. */
function Lines({ text }: { text: string }) {
  const lines = text.endsWith("\n") ? text.slice(0, -1).split("\n") : text.split("\n");
  return (
    <ol className="fk-lines">
      {lines.map((line, index) => {
        // Indentation becomes padding, so a wrapped line never breaks after its spaces.
        const indent = line.length - line.trimStart().length;
        return (
          <li key={`${String(index)}:${line}`}>
            <code style={{ paddingLeft: `${String(indent + 4)}ch` }}>{line.trimStart()}</code>
          </li>
        );
      })}
    </ol>
  );
}

/** What each pattern of the matcher sample shows, in words. */
const PATTERN_NOTES: Record<string, string> = {
  star: "A single star stays inside one folder, so only the file at the top matches.",
  globstar: "The double star stands for any number of folders, including none.",
  braces: "Braces list alternatives: a name ending in .ts or in .tsx, anywhere below src.",
  extglob: "The !(…) group accepts any name except the one inside it, so the test file drops out.",
};

/** Five paths and the patterns that select them. */
export function MatcherSpecimen() {
  const [id, setId] = useState("globstar");
  const current = matcher.patterns.find((entry) => entry.id === id) ?? matcher.patterns[0];
  if (!current) throw new Error("The matcher sample has no patterns.");
  const count = current.matches.filter(Boolean).length;
  return (
    <MonoWide>
      <div className="fk-toggles">
        <MonoSegmented
          label="Pattern"
          value={current.id}
          options={matcher.patterns.map((entry) => ({
            value: entry.id,
            label: <code>{entry.pattern}</code>,
          }))}
          onChange={setId}
        />
      </div>
      <MonoFacing className="fk-single">
        <MonoSheet
          label={capitalized(`${spelled(matcher.paths.length)} paths, ${spelled(count)} selected`)}
          meta={current.pattern}
        >
          <ul className="fk-paths" aria-live="polite">
            {matcher.paths.map((path, index) => {
              const selected = current.matches[index] === true;
              return (
                <li key={path} data-selected={selected ? "" : undefined}>
                  <code>{path}</code>
                  <span>{selected ? "selected" : "not selected"}</span>
                </li>
              );
            })}
          </ul>
        </MonoSheet>
      </MonoFacing>
      <MonoCaption>
        Real output of ferralk-glob {renderedWith}, which checks each path the way the walker reads
        a pattern. {PATTERN_NOTES[current.id]}
      </MonoCaption>
    </MonoWide>
  );
}

const QUERY_LABELS: Record<string, string> = {
  scoped: "Named folders",
  unscoped: "Whole project",
  unignored: "Without ignore rules",
};

function querySource(pattern: string, gitIgnore: boolean) {
  return [
    'let result = Walker::new(".")',
    `    .include("${pattern}")?`,
    `    .respect_git_ignore(${String(gitIgnore)})`,
    "    .options(WalkOptions::default().files_only(true).sort(true))",
    "    .collect()?;",
  ].join("\n");
}

type WalkQuery = (typeof walk.queries)[number];

/** What one search found, and the errors it met on the way. */
function WalkResult({ query }: { query: WalkQuery }) {
  return (
    <MonoSheet
      label={capitalized(`${spelled(query.paths.length)} files found`)}
      meta="result.entries()"
    >
      <ul className="fk-found">
        {query.paths.map((path) => (
          <li key={path}>
            <code>{path}</code>
          </li>
        ))}
      </ul>
      <p className="fk-sub">Errors, from result.errors()</p>
      {query.errors.length === 0 ? (
        <p className="fk-clear">None.</p>
      ) : (
        <ul className="fk-errors">
          {query.errors.map((error) => (
            <li key={error.error}>
              <code>{error.error}</code>
              {error.cause && <span>{error.cause}</span>}
            </li>
          ))}
        </ul>
      )}
    </MonoSheet>
  );
}

/** The sample project and its ignore rules, folded under the result. */
function ProjectFiles() {
  return (
    <details className="fk-details">
      <summary>Show the project’s files and its .gitignore</summary>
      <MonoFacing className="fk-facing">
        <MonoSheet label="The project" meta={`${String(walk.files.length)} files`}>
          <Lines text={walk.files.join("\n")} />
        </MonoSheet>
        <MonoSheet label="The ignore rules" meta=".gitignore">
          <Lines text={walk.gitIgnore} />
        </MonoSheet>
      </MonoFacing>
    </details>
  );
}

/** The same project searched three ways, with the code beside its result. */
export function WalkSpecimen() {
  const [id, setId] = useState("scoped");
  const current = walk.queries.find((entry) => entry.id === id) ?? walk.queries[0];
  if (!current) throw new Error("The walk sample has no queries.");
  return (
    <MonoWide>
      <div className="fk-toggles">
        <MonoSegmented
          label="Search"
          value={current.id}
          options={walk.queries.map((entry) => ({
            value: entry.id,
            label: QUERY_LABELS[entry.id] ?? entry.id,
          }))}
          onChange={setId}
        />
      </div>
      <MonoFacing className="fk-facing" aria-live="polite">
        <MonoSheet label="The search, in Rust" meta="Walker">
          <Lines text={querySource(current.pattern, current.gitIgnore)} />
        </MonoSheet>
        <WalkResult query={current} />
      </MonoFacing>
      <MonoCaption>
        Real output of ferralk {renderedWith} in a small project built for this page. Its{" "}
        <code>{walk.unreadable}</code> folder cannot be read, like a database folder that belongs to
        another user. A search that has to look inside reports that next to its result; a search
        whose pattern names other folders never tries.
      </MonoCaption>
      <ProjectFiles />
    </MonoWide>
  );
}

/** The byte names as a table: bytes, lossy text, and each pattern's verdict. */
function BytesTable() {
  return (
    <table className="mono-table fk-bytes">
      <thead>
        <tr>
          <th scope="col">File name</th>
          <th scope="col">Bytes on disk</th>
          <th scope="col">As lossy text</th>
          {bytes.patterns.map((entry) => (
            <th scope="col" key={entry.pattern}>
              <code>{entry.pattern}</code>
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {bytes.names.map((name, index) => (
          <tr key={name.escaped}>
            <th scope="row">
              <code>{name.escaped}</code>
              <small>{name.encoding}</small>
            </th>
            <td>
              <code>{name.hex}</code>
            </td>
            <td>
              <code>{name.lossy}</code>
            </td>
            {bytes.patterns.map((entry) => (
              <td key={entry.pattern}>
                {entry.matches[index] === true ? "selected" : <span>not selected</span>}
              </td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** Three names that differ in one letter, as bytes, as lossy text and as matched. */
export function BytesSpecimen() {
  return (
    <MonoWide>
      <div
        className="fk-scroll"
        role="region"
        aria-label="Three file names matched as bytes"
        tabIndex={0}
      >
        <BytesTable />
      </div>
      <MonoCaption>
        Real output of ferralk-glob {renderedWith}, matching the names as bytes in memory.{" "}
        <code>\xE9</code> is one byte that is not valid UTF-8. The lossy column is what a program
        sees after Rust’s <code>String::from_utf8_lossy</code>: two different files, one name.
      </MonoCaption>
    </MonoWide>
  );
}
