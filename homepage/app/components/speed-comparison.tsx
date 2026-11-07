import { type ReactNode, useState } from "ardo/react";
import {
  MonoBarChart,
  type MonoBarRow,
  MonoSegmented,
  MonoTableView,
} from "ferramenta-family/monografie";

import { samples } from "./home-data";

/* Time for the same work, relative to Ferralk, from the published tables in
   docs/benchmark-evidence.md via app/data/home-samples.json. */

const { speed } = samples;
type TaskId = "match" | "roots" | "tree";
const isTask = (value: string): value is TaskId =>
  value === "match" || value === "roots" || value === "tree";

const threads = `${String(speed.measured.threads)} threads`;
/** The task in words, and what each tool is, in the order of the toggles. */
const TASKS: Record<TaskId, { label: string; phrase: string; own: string }> = {
  roots: {
    label: "Search named folders",
    phrase: "finding the sources under src and packages",
    own: `Rust, ${threads}`,
  },
  tree: {
    label: "Search every folder",
    phrase: "finding every TypeScript file in the project",
    own: `Rust, ${threads}`,
  },
  match: {
    label: "Match one path",
    phrase: "checking one path against a pattern",
    own: "Rust",
  },
};
const ORDER: TaskId[] = ["roots", "tree", "match"];
const SUBS: Record<TaskId, Record<string, string>> = {
  roots: {
    ignore: `Rust, ${threads}, pruning written by hand`,
    zlob: `Zig, ${threads}`,
    jwalk: `Rust, ${threads}`,
  },
  tree: {
    jwalk: `Rust, ${threads}`,
    zlob: `Zig, ${threads}`,
    ignore: `Rust, ${threads}, globs as overrides`,
  },
  match: { zlob: "Zig", globset: "Rust", "fast-glob": "Rust" },
};

function task(id: TaskId) {
  const found = speed.tasks.find((entry) => entry.id === id);
  if (!found) throw new Error(`The speed data has no ${id} task.`);
  return found;
}

const time = (value: number, unit: string) =>
  `${value.toLocaleString("en-US", { maximumFractionDigits: 2 })} ${unit}`;

/** A round end for the scale, past the slowest bar, with ticks at its middle and end. */
function scaleFor(id: TaskId) {
  const slowest = Math.max(...task(id).rows.map((row) => row.ratio));
  const scale = [2, 3, 4, 5, 6, 8, 10, 15, 20, 30, 40].find((step) => step >= slowest) ?? 50;
  const ticks = [...new Set([1, scale / 2, scale])].filter((tick) => tick >= 1);
  return { scale, ticks };
}

function rowsFor(id: TaskId): MonoBarRow[] {
  return [
    { id: "ferralk", label: "Ferralk", sub: TASKS[id].own, value: 1, own: true },
    ...task(id).rows.map((row) => ({
      id: row.id,
      label: row.label,
      sub: SUBS[id][row.id] ?? "",
      value: row.ratio,
    })),
  ];
}

function describeFor(id: TaskId) {
  const { ferralk, rows, unit } = task(id);
  return function describe(row: MonoBarRow, cut: boolean) {
    if (row.own === true) {
      return (
        <>
          <b>Ferralk</b> is the baseline: 1× for {TASKS[id].phrase}, {time(ferralk, unit)}.
        </>
      );
    }
    const measured = rows.find((entry) => entry.id === row.id);
    return (
      <>
        <b>{row.label}</b> needed {row.value.toFixed(2)}× as long as Ferralk for {TASKS[id].phrase}
        {measured ? `, ${time(measured.value, unit)}` : ""}
        {cut ? "; its bar runs past the end of the scale." : "."}
      </>
    );
  };
}

/** Every figure of the chart as a table, with the measured times. */
function Figures() {
  return (
    <MonoTableView>
      <table className="mono-table">
        <thead>
          <tr>
            <th scope="col">Task</th>
            <th scope="col">Tool</th>
            <th scope="col" className="mono-num">
              Time
            </th>
            <th scope="col" className="mono-num">
              Relative
            </th>
          </tr>
        </thead>
        <tbody>
          {ORDER.flatMap((id) => {
            const { ferralk, rows, unit } = task(id);
            return [
              <tr key={`${id}-ferralk`}>
                <td>{TASKS[id].label}</td>
                <td>Ferralk</td>
                <td className="mono-num">{time(ferralk, unit)}</td>
                <td className="mono-num">1×</td>
              </tr>,
              ...rows.map((row) => (
                <tr key={`${id}-${row.id}`}>
                  <td>{TASKS[id].label}</td>
                  <td>
                    {row.label} <small>({SUBS[id][row.id]})</small>
                  </td>
                  <td className="mono-num">{time(row.value, unit)}</td>
                  <td className="mono-num">{row.ratio.toFixed(2)}×</td>
                </tr>
              )),
            ];
          })}
        </tbody>
      </table>
    </MonoTableView>
  );
}

export type SpeedComparisonProps = {
  /** Text and links under the scale. */
  after: ReactNode;
  /** The measuring conditions beside the chart. */
  conditions: ReactNode;
};

export function SpeedComparison({ after, conditions }: SpeedComparisonProps) {
  const [id, setId] = useState<TaskId>("roots");
  const { scale, ticks } = scaleFor(id);
  return (
    <MonoBarChart
      label={`Time for ${TASKS[id].phrase}, relative to Ferralk`}
      title="Time for the same work"
      description="Ferralk = 1×, the line through every row. A bar past it took longer."
      toggles={
        <MonoSegmented
          label="Task"
          value={id}
          options={ORDER.map((value) => ({ value, label: TASKS[value].label }))}
          onChange={(value) => {
            if (isTask(value)) setId(value);
          }}
        />
      }
      rows={rowsFor(id)}
      scale={scale}
      ticks={ticks}
      describe={describeFor(id)}
      after={
        <>
          {after}
          <Figures />
        </>
      }
      conditions={conditions}
    />
  );
}
