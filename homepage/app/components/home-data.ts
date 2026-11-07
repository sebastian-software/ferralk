import { family } from "ferramenta-family/registry";

import homeSamples from "../data/home-samples.json";

/* Everything the homepage shows is generated, never edited by hand: the
   specimens and speed figures in app/data/home-samples.json
   (scripts/render-home-samples.mjs), the corpus counts in app/data/corpus.json
   (scripts/write-site-data.mjs, at build time). */
export const samples = homeSamples;
export { default as corpus } from "../data/corpus.json";

/** The crate version that rendered the specimens. */
export const renderedWith = samples.renderedWith.ferralk;

const member = family.find((entry) => entry.name === "ferralk");
if (!member) throw new Error("Ferralk is missing from the family registry.");
export const ferralk = member;

const application = family.find((entry) => entry.name === "palamedes");
if (!application?.runsOn?.includes("ferralk")) {
  throw new Error("The registry no longer says that Palamedes runs on Ferralk.");
}
export const palamedes = application;

/** A registry note by its title, for the margin. */
export function registryNote(title: string) {
  const note = ferralk.editorial?.notes.find((entry) => entry.title === title);
  if (!note) throw new Error(`The registry has no Ferralk note "${title}".`);
  return note;
}

const NUMBERS = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"];
/** Small counts in words, as the reading text writes them. */
export const spelled = (count: number) => NUMBERS[count] ?? count.toLocaleString("en-US");

/** The first letter in lower case, to set a registry phrase inside a sentence. */
export const lowerFirst = (text: string) => `${text.charAt(0).toLowerCase()}${text.slice(1)}`;
