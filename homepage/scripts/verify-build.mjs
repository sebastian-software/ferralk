/* eslint-disable security/detect-non-literal-fs-filename -- Built files and route targets stay inside the verified site output. */
import { access, readdir, readFile } from "node:fs/promises";
import { extname, join, normalize, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { expectedPages } from "./site-pages.mjs";

const outputDirectory = new URL("../build/client/", import.meta.url);
const read = async (page) => {
  const html = await readFile(new URL(page, outputDirectory), "utf8");
  return html.replaceAll(/<!--.*?-->/g, "");
};
const pageFiles = await Promise.all(expectedPages.map(async (page) => [page, await read(page)]));
const linkCount = await verifyRenderedPages(pageFiles);
verifyFocusableMarkdownCodeBlocks(await read("guide/getting-started/index.html"));
verifyFocusableMarkdownTables(await read("reference/patterns/index.html"));
const { version } = JSON.parse(
  await readFile(new URL("../app/data/site.json", import.meta.url), "utf8"),
);
for (const [page, html] of pageFiles) {
  for (const fragment of [
    'class="mono-header"',
    'class="mono-footer"',
    `<span class="mono-release" title="Current release">v${version}</span>`,
    "https://ferramenta.dev",
  ]) {
    if (!html.includes(fragment)) throw new Error(`${page} is missing ${fragment}.`);
  }
  if (html.includes("site.css")) throw new Error(`${page} still uses project-specific CSS.`);
}
verifyHomepage(pageFiles.find(([page]) => page === "index.html")?.[1] ?? "");
verifyDocsPages(pageFiles);
await verifyNoFerra(pageFiles);
await verifyScopedCss();
console.log(
  `Verified ${expectedPages.length} pages and ${linkCount} internal links and fragments.`,
);

/** The homepage reads on the Monografie page and keeps its published anchors. */
function verifyHomepage(html) {
  if (!html.includes('class="mono fk-home"')) {
    throw new Error("The homepage should use the Monografie page.");
  }
  for (const id of ["overview", "example", "names", "speed", "verification", "start"]) {
    if (!html.includes(`id="${id}"`)) {
      throw new Error(`Preserve the published homepage anchor #${id}.`);
    }
  }
  if (!html.includes('data-product="ferralk"')) {
    throw new Error("The homepage opening should show Ferralk's own instrument.");
  }
}

/** Documentation pages read in the Monografie reading layout inside Ardo's shell. */
function verifyDocsPages(pages) {
  for (const [page, html] of pages) {
    if (page === "index.html") continue;
    for (const fragment of [
      'class="mono mono-docs fk-docs"',
      "mono-ardo-shell",
      'class="mono-reading"',
      'class="mono-section-rail"',
      'class="mono-guide-navigation"',
      'class="mono-prose"',
    ]) {
      if (!html.includes(fragment)) throw new Error(`${page} is missing ${fragment}.`);
    }
  }
}

/**
 * No page renders a Ferra class or loads the Ferra stylesheets: the built
 * pages, the bundled CSS and the site's imports are all on Monografie.
 */
async function verifyNoFerra(pages) {
  for (const [page, html] of pages) {
    const ferra = htmlTags(html).flatMap((tag) =>
      retiredFerraClasses(decodeHtml(attribute(tag.raw, "class") ?? "").split(/\s+/)),
    );
    if (ferra.length > 0) throw new Error(`${page} renders Ferra classes: ${ferra.join(", ")}.`);
  }
  await verifyNoFerraStyles();
  await verifyNoFerraImports();
}

/** The Monografie entry's identity classes are the only ferra- names a page may keep. */
function retiredFerraClasses(names) {
  const kept = new Set(["ferra-mark", "ferra-instrument", "ferra-workshop-logo"]);
  return names.filter((name) => name.startsWith("ferra-") && !kept.has(name));
}

async function verifyNoFerraStyles() {
  const assets = await readdir(new URL("assets/", outputDirectory), { recursive: true });
  for (const file of assets.filter((name) => name.endsWith(".css"))) {
    const css = await readFile(new URL(`assets/${file}`, outputDirectory), "utf8");
    const ferra = retiredFerraClasses([...css.matchAll(/\.(ferra-[\w-]+)/g)].map((m) => m[1]));
    if (ferra.length > 0) {
      throw new Error(`assets/${file} bundles Ferra styles: ${[...new Set(ferra)].join(", ")}.`);
    }
  }
}

async function verifyNoFerraImports() {
  const sources = await readdir(new URL("../app/", import.meta.url), { recursive: true });
  for (const file of sources.filter((name) => /\.(?:tsx?|mdx|css)$/.test(name))) {
    const source = await readFile(new URL(`../app/${file}`, import.meta.url), "utf8");
    if (/ferramenta-family\/(?:ferra(?:-ardo)?\.css|ferra)["']/.test(source)) {
      throw new Error(`app/${file} imports the Ferra entry or its stylesheets.`);
    }
  }
}

/** A selector list split at its top-level commas, so `:is(a, b)` stays whole. */
function selectorList(prelude) {
  const selectors = [""];
  let depth = 0;
  for (const char of prelude) {
    if (char === "(") depth += 1;
    if (char === ")") depth -= 1;
    if (char === "," && depth === 0) selectors.push("");
    else selectors[selectors.length - 1] += char;
  }
  return selectors;
}

/**
 * Shared presentation lives in ferramenta-family. The site styles only its own
 * homepage specimens and shell seams, next to their components, under fk-.
 */
async function verifyScopedCss() {
  const appFiles = await readdir(new URL("../app/", import.meta.url), { recursive: true });
  for (const file of appFiles.filter((name) => name.endsWith(".css"))) {
    if (!file.startsWith("components/")) {
      throw new Error(
        `Keep ${file} beside its specimen in app/components/, or in ferramenta-family.`,
      );
    }
    const css = await readFile(new URL(`../app/${file}`, import.meta.url), "utf8");
    const preludes = css
      .replaceAll(/\/\*[\s\S]*?\*\//g, "")
      .split("{")
      .slice(0, -1)
      .map((chunk) =>
        chunk.slice(Math.max(chunk.lastIndexOf("}"), chunk.lastIndexOf(";")) + 1).trim(),
      )
      .filter((prelude) => !prelude.startsWith("@"));
    for (const selector of preludes.flatMap((prelude) => selectorList(prelude))) {
      if (!selector.includes(".fk-")) {
        throw new Error(`${file}: scope "${selector.trim()}" to an fk- class.`);
      }
    }
  }
}

async function verifyRenderedPages(pages) {
  const context = {
    outputPath: fileURLToPath(outputDirectory),
    pageIds: new Map(),
  };
  for (const [page, html] of pages) {
    const tags = htmlTags(html);
    assertPageStructure(page, tags);
    context.pageIds.set(page, collectIds(tags));
  }

  let internalLinks = 0;
  for (const [page, html] of pages) internalLinks += await verifyPageLinks(page, html, context);
  return internalLinks;
}

function verifyFocusableMarkdownTables(html) {
  const tables = htmlTags(html).filter((tag) => tag.name === "table");
  if (tables.length === 0) {
    throw new Error("The rendered API guide has no Markdown tables to verify.");
  }

  const accessible = [
    ...html.matchAll(/<div\s[^>]*role="region"[^>]*tabindex="0"[^>]*>[\s\S]*?<table\b/g),
  ];
  if (accessible.length !== tables.length) {
    throw new Error("Every Markdown table must have a focusable scrolling region.");
  }
}

function verifyFocusableMarkdownCodeBlocks(html) {
  const blocks = htmlTags(html).filter((tag) => tag.name === "pre");
  if (blocks.length === 0) {
    throw new Error("The rendered getting-started guide has no Markdown code blocks to verify.");
  }

  const missingTabIndex = blocks.filter((block) => attribute(block.raw, "tabindex") !== "0");
  if (missingTabIndex.length > 0) {
    throw new Error(
      `The rendered getting-started guide has ${missingTabIndex.length} Markdown code block(s) without keyboard focus.`,
    );
  }
}

function assertPageStructure(page, tags) {
  const h1Count = tags.filter((tag) => tag.name === "h1").length;
  if (h1Count !== 1) throw new Error(`${page} must contain exactly one h1; found ${h1Count}.`);
  const mainCount = tags.filter(
    (tag) => tag.name === "main" || attribute(tag.raw, "role") === "main",
  ).length;
  if (mainCount !== 1)
    throw new Error(`${page} must contain exactly one main landmark; found ${mainCount}.`);
}

async function verifyPageLinks(page, html, context) {
  let links = 0;
  for (const tag of htmlTags(html)) {
    if (tag.name === "a" && (await verifyInternalLink(page, tag, context))) links++;
  }
  return links;
}

async function verifyInternalLink(page, tag, context) {
  const rawHref = attribute(tag.raw, "href");
  if (rawHref === undefined) return false;
  const href = decodeHtml(rawHref).trim();
  if (href === "") return false;

  const sourcePath = page === "index.html" ? "/" : `/${page.replaceAll(/index\.html$/g, "")}`;
  const targetUrl = new URL(href, new URL(sourcePath, "https://ferralk.dev"));
  if (targetUrl.origin !== "https://ferralk.dev") return false;

  const targetPath = resolveTargetPath({
    targetUrl,
    href,
    sourcePage: page,
    outputPath: context.outputPath,
  });
  const absolutePath = resolve(context.outputPath, targetPath);
  await access(absolutePath).catch(() => {
    throw new Error(`${page} links to a missing built file: ${href} (${targetPath})`);
  });
  await verifyFragment({ page, targetPath, targetUrl, absolutePath, href, context });
  return true;
}

function resolveTargetPath({ targetUrl, href, sourcePage, outputPath }) {
  const decodedPath = decodeURIComponent(targetUrl.pathname);
  let targetPath = normalize(decodedPath.replaceAll(/^\/+/g, ""));
  if (targetPath === "" || decodedPath.endsWith("/") || !extname(targetPath))
    targetPath = join(targetPath, "index.html");
  const absolutePath = resolve(outputPath, targetPath);
  const relativePath = relative(outputPath, absolutePath);
  if (relativePath === ".." || relativePath.startsWith(`..${sep}`))
    throw new Error(`${sourcePage} links outside the built site: ${href}`);
  return targetPath;
}

async function verifyFragment({ page, targetPath, targetUrl, absolutePath, href, context }) {
  const fragment = decodeURIComponent(targetUrl.hash.slice(1));
  if (!fragment || !targetPath.endsWith(".html")) return;
  let ids = context.pageIds.get(targetPath);
  if (ids === undefined) {
    const html = await readFile(absolutePath, "utf8");
    ids = collectIds(htmlTags(html));
    context.pageIds.set(targetPath, ids);
  }
  if (!ids.has(fragment)) throw new Error(`${page} links to a missing fragment: ${href}`);
}

function collectIds(tags) {
  return new Set(
    tags
      .map((tag) => attribute(tag.raw, "id"))
      .filter((id) => id !== undefined)
      .map((id) => decodeHtml(id)),
  );
}

function htmlTags(html) {
  return [...html.matchAll(/<([a-z][a-z0-9:-]*)\b[^<>]*>/gi)].map((match) => ({
    name: match[1].toLowerCase(),
    raw: match[0],
    index: match.index,
  }));
}

function attribute(tag, name) {
  const wantedName = name.toLowerCase();
  const cursor = { index: skipTagName(tag) };
  while (cursor.index < tag.length) {
    skipWhitespace(tag, cursor);
    if (tag[cursor.index] === "/" || tag[cursor.index] === ">") break;
    const attributeName = readAttributeName(tag, cursor);
    if (attributeName === "") continue;
    const value = readAttributeValue(tag, cursor);
    if (attributeName === wantedName) return value;
  }
}

function skipTagName(tag) {
  let index = 1;
  while (index < tag.length && !/[\s/>]/.test(tag[index])) index++;
  return index;
}

function skipWhitespace(tag, cursor) {
  while (cursor.index < tag.length && /\s/.test(tag[cursor.index])) cursor.index++;
}

function readAttributeName(tag, cursor) {
  const start = cursor.index;
  while (cursor.index < tag.length && !/[\s=/>]/.test(tag[cursor.index])) cursor.index++;
  if (start === cursor.index) {
    cursor.index++;
    return "";
  }
  return tag.slice(start, cursor.index).toLowerCase();
}

function readAttributeValue(tag, cursor) {
  skipWhitespace(tag, cursor);
  if (tag[cursor.index] !== "=") return "";
  cursor.index++;
  skipWhitespace(tag, cursor);
  return readQuotedValue(tag, cursor) ?? readUnquotedValue(tag, cursor);
}

function readQuotedValue(tag, cursor) {
  const quote = tag[cursor.index];
  if (quote !== '"' && quote !== "'") return;
  const start = ++cursor.index;
  while (cursor.index < tag.length && tag[cursor.index] !== quote) cursor.index++;
  const value = tag.slice(start, cursor.index);
  cursor.index++;
  return value;
}

function readUnquotedValue(tag, cursor) {
  const start = cursor.index;
  while (cursor.index < tag.length && !/[\s>]/.test(tag[cursor.index])) cursor.index++;
  return tag.slice(start, cursor.index);
}

function decodeHtml(value) {
  return value
    .replaceAll(/&amp;/gi, "&")
    .replaceAll(/&quot;/gi, '"')
    .replaceAll(/&#39;|&apos;/gi, "'")
    .replaceAll(/&lt;/gi, "<")
    .replaceAll(/&gt;/gi, ">")
    .replaceAll(/&#(\d+);?/g, (_, digits) => String.fromCodePoint(Number(digits)))
    .replaceAll(/&#x([\da-f]+);?/gi, (_, digits) =>
      String.fromCodePoint(Number.parseInt(digits, 16)),
    );
}
