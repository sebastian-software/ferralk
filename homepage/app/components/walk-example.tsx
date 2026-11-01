import { useState } from "ardo/react";
import { ArdoLink as Link } from "ardo/runtime";
import {
  FerraArticle,
  FerraCode,
  FerraCodePair,
  FerraSelect,
  FerraSpecimen,
} from "ferramenta-family/ferra";

import fixture from "../data/walk-sample.json";

export function WalkExample() {
  const [selected, setSelected] = useState("scoped");
  const sample = fixture.samples.find(({ id }) => id === selected);
  if (!sample) throw new Error("Unknown walk example.");
  const source = `let result = Walker::new(".")\n    .include("${sample.pattern}")?\n    .respect_git_ignore(true)\n    .options(WalkOptions::default().files_only(true).sort(true))\n    .collect()?;\n\nfor entry in result.entries() {\n    println!("{}", entry.path().display());\n}\nfor error in result.errors() {\n    eprintln!("{error}");\n}`;
  return (
    <FerraArticle
      id="example"
      title="Let the pattern narrow the search."
      introduction="Choose a query over the same small project. A scoped pattern names the only roots that can match; a broad pattern also finds the legacy source under public."
      meta={<Link to="/guide/usage/">Matching, defaults and error handling</Link>}
    >
      <FerraSelect
        label="File query"
        value={selected}
        options={fixture.samples.map(({ id, label }) => ({ value: id, label }))}
        onChange={(event) => {
          setSelected(event.target.value);
        }}
      />
      <FerraCodePair>
        <FerraCode caption="The query · Rust" file="find.rs">
          {source}
        </FerraCode>
        <WalkResult sample={sample} />
      </FerraCodePair>
      <details>
        <summary>See the project files used in this example</summary>
        <FerraCode caption="Project fixture">{fixture.files.join("\n")}</FerraCode>
      </details>
      <p>
        These results come from the repository’s{" "}
        <a href="https://github.com/sebastian-software/ferralk/blob/main/crates/ferralk/examples/find.rs">
          Rust example
        </a>
        , run against the fixture above. The walkthrough checks them again during CI.
      </p>
    </FerraArticle>
  );
}

function WalkResult({ sample }: { sample: (typeof fixture.samples)[number] }) {
  return (
    <FerraSpecimen caption={`Native Rust result · ${sample.paths.length} files`}>
      <ul>
        {sample.paths.map((path) => (
          <li key={path}>
            <code>{path}</code>
          </li>
        ))}
      </ul>
      <p>
        The fixture’s <code>.gitignore</code> excludes <code>node_modules/</code> and{" "}
        <code>dist/</code>. Wildcard includes skip hidden names by default.
      </p>
    </FerraSpecimen>
  );
}
