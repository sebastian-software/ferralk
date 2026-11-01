/* eslint-disable security/detect-non-literal-fs-filename -- Vite and axe paths are package-owned; pages come from the site route list. */
import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { preview } from "vite";

import { expectedPages, pagePath } from "./site-pages.mjs";

const homepageRoot = fileURLToPath(new URL("..", import.meta.url));
const require = createRequire(import.meta.url);
const axeSource = await readFile(require.resolve("axe-core/axe.min.js"), "utf8");
const requestedPort = Number(process.env.FERRALK_PREVIEW_PORT ?? 0);
assert(
  Number.isInteger(requestedPort) && requestedPort >= 0 && requestedPort <= 65_535,
  "FERRALK_PREVIEW_PORT must be a valid TCP port",
);
const selectors = {
  nav: ".ferra-header > .ferra-nav",
  menu: 'button[aria-label="Toggle menu"]',
  summary: 'button[aria-label="Toggle menu"]',
  flyout: '[role="dialog"]',
  search: 'button[aria-label="Search"]',
};

let browser;
let previewServer;
try {
  previewServer = await preview({
    configFile: false,
    root: homepageRoot,
    logLevel: "silent",
    base: "/",
    build: { outDir: "build/client" },
    preview: {
      host: "127.0.0.1",
      port: requestedPort,
      strictPort: requestedPort !== 0,
    },
  });
  const address = previewServer.httpServer.address();
  assert(
    address && typeof address !== "string",
    "Vite preview did not expose a listening TCP address",
  );
  const origin = `http://127.0.0.1:${address.port}`;
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ colorScheme: "light" });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(error.message));
  for (const pageFile of expectedPages) {
    for (const viewport of [
      { name: "phone-small", width: 320, height: 760 },
      { name: "phone-compact", width: 375, height: 812 },
      { name: "phone", width: 390, height: 844 },
      { name: "phone-wide", width: 640, height: 900 },
      { name: "tablet", width: 768, height: 1024 },
      { name: "landscape", width: 844, height: 390 },
      { name: "tablet-wide", width: 1024, height: 900 },
      { name: "desktop", width: 1440, height: 900 },
    ]) {
      await verifyViewport({ page, pageFile, viewport, origin });
    }
  }

  await verifyExample(page, origin);
  await verifyNavigation(page, origin);
  await verifySearch(page, origin);
  await page.emulateMedia({ reducedMotion: "reduce" });
  const motion = await page
    .locator(".ferra-page, .ferra-page *")
    .evaluateAll((elements) =>
      elements
        .map((element) => getComputedStyle(element).animationDuration)
        .filter((value) => value !== "0s"),
    );
  assert.deepEqual(motion, [], "Reduced motion still animates the page");
  assert.deepEqual(runtimeErrors, [], "The site raised browser runtime errors");
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await capture(page, origin);
  console.log(
    "Verified all pages at eight widths, native file-search choices, navigation, search, focus return, reduced motion and runtime errors.",
  );
} finally {
  await browser?.close();
  await previewServer?.close();
}

async function verifyExample(page, origin) {
  await page.goto(new URL("/", origin).href, { waitUntil: "load" });
  const example = page.locator("#example");
  const query = example.getByRole("combobox", { name: "File query", exact: true });
  await example.getByText("Native Rust result · 3 files", { exact: true }).waitFor();
  await query.selectOption("unscoped");
  await example.getByText("Native Rust result · 4 files", { exact: true }).waitFor();
  assert.match(await example.locator(".ferra-specimen").textContent(), /public\/legacy\.ts/);
  await query.selectOption("scoped");
  await example.getByText("Native Rust result · 3 files", { exact: true }).waitFor();
  assert.doesNotMatch(await example.locator(".ferra-specimen").textContent(), /public\/legacy\.ts/);
  await example.locator("summary").click();
  assert.match(await example.textContent(), /node_modules\/vendor\/index\.ts/);
}
async function verifyNavigation(page, origin) {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(new URL("/guide/getting-started/", origin).href, { waitUntil: "load" });
  await page.getByRole("heading", { name: "Getting started", level: 1, exact: true }).waitFor();
  const menu = page.locator(selectors.menu);
  await focusMenuByKeyboard(page, menu);
  await page.keyboard.press("Enter");
  await page.getByRole("dialog").waitFor();
  await page.keyboard.press("Escape");
  await page.getByRole("dialog").waitFor({ state: "detached" });
  assert(
    await menu.evaluate((element) => element === document.activeElement),
    "Menu focus was not restored",
  );
  await page.keyboard.press("Enter");
  await page.getByRole("dialog").getByRole("link", { name: "Using Ferralk", exact: true }).click();
  await page.waitForURL((url) => url.pathname.startsWith("/guide/usage"));
  await page.getByRole("dialog").waitFor({ state: "detached" });
  await verifyLandscape(page, origin);
}
async function focusMenuByKeyboard(page, menu) {
  let reached = false;
  for (let i = 0; i < 40; i++) {
    await page.keyboard.press("Tab");
    if (await menu.evaluate((element) => element === document.activeElement)) {
      reached = true;
      break;
    }
  }
  assert(reached, "Keyboard navigation does not reach the menu");
}
async function verifyLandscape(page, origin) {
  await page.setViewportSize({ width: 844, height: 390 });
  await page.goto(new URL("/reference/patterns/", origin).href, { waitUntil: "load" });
  const table = page
    .locator('div[role="region"][tabindex="0"]')
    .filter({ has: page.locator("table") })
    .first();
  await table.focus();
  assert(
    await table.evaluate((element) => element === document.activeElement),
    "Markdown tables are not keyboard focusable",
  );
  await page.locator(selectors.menu).focus();
  await page.keyboard.press("Enter");
  const lastLink = page.getByRole("dialog").getByRole("link").last();
  await lastLink.focus();
  const bounds = await lastLink.boundingBox();
  assert(
    bounds && bounds.y >= 0 && bounds.y + bounds.height <= 390,
    "Landscape menu link leaves the viewport",
  );
  await page.keyboard.press("Escape");
}
async function verifySearch(page, origin) {
  for (const width of [320, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(new URL("/", origin).href, { waitUntil: "load" });
    const search = page.locator(selectors.search);
    await search
      .locator("kbd")
      .first()
      .evaluate((element) => {
        element.textContent = "Ctrl";
      });
    await search.hover();
    const hint = await search.locator('span[aria-hidden="true"]').boundingBox();
    assert(hint && hint.x >= 0 && hint.x + hint.width <= width, "Search hint leaves the viewport");
    await search.click();
    const dialog = page.getByRole("dialog");
    await dialog.locator("input").fill("consumer pattern contract");
    await dialog.locator('a[href*="/reference/patterns"]').first().waitFor();
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert(
      await search.evaluate((element) => element === document.activeElement),
      "Search focus was not restored",
    );
  }
}
async function capture(page, origin) {
  await mkdir(join(tmpdir(), "ferralk-review"), { recursive: true });
  for (const [path, label] of [
    ["/", "home"],
    ["/guide/getting-started/", "guide"],
    ["/reference/patterns/", "reference"],
    ["/evidence/benchmarks/", "performance"],
  ]) {
    for (const width of [390, 1440]) {
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(new URL(path, origin).href, { waitUntil: "load" });
      await page.screenshot({
        path: join(tmpdir(), `ferralk-review/${label}-${width}.png`),
        fullPage: true,
      });
    }
  }
}

async function verifyViewport({ page, pageFile, viewport, origin }) {
  await page.setViewportSize({ width: viewport.width, height: viewport.height });
  await page.goto(new URL(pagePath(pageFile), origin).href, { waitUntil: "load" });
  const layout = await page.evaluate(() => ({
    width: globalThis.document.documentElement.clientWidth,
    pageWidth: Math.max(
      globalThis.document.documentElement.scrollWidth,
      globalThis.document.body.scrollWidth,
    ),
  }));
  assert(
    layout.pageWidth <= layout.width + 1,
    `${pageFile} has horizontal page overflow at ${viewport.name} width ${viewport.width}px: ${layout.pageWidth}px`,
  );

  const navDisplay = await page
    .locator(selectors.nav)
    .evaluate((element) => getComputedStyle(element).display);
  const menuDisplay = await page
    .locator(selectors.menu)
    .evaluate((element) => getComputedStyle(element).display);
  assert(
    await page.locator(selectors.search).isVisible(),
    "The site should keep search visible at each checked viewport",
  );
  if (viewport.name === "desktop") {
    assert.notEqual(navDisplay, "none", `${pageFile} hides desktop section navigation`);
    assert.equal(menuDisplay, "none", `${pageFile} shows the phone navigation on desktop`);
  } else {
    assert.equal(
      navDisplay,
      "none",
      `${pageFile} shows desktop navigation at ${viewport.name} width`,
    );
    assert.notEqual(
      menuDisplay,
      "none",
      `${pageFile} hides the phone navigation at ${viewport.name} width`,
    );
  }
  await verifyAccessibility(page, pageFile, viewport);
  console.log(`Verified ${pageFile} at ${viewport.name} (${viewport.width}px).`);
}
async function verifyAccessibility(page, pageFile, viewport) {
  await page.addScriptTag({ content: axeSource });
  const results = await page.evaluate(async () =>
    globalThis.axe.run(globalThis.document, {
      runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"] },
    }),
  );
  if (results.violations.length === 0) return;
  const details = results.violations
    .map((violation) => {
      const targets = violation.nodes
        .map((node) => `  ${node.target.join(", ")}: ${node.failureSummary}`)
        .join("\n");
      return `${violation.id} (${violation.impact}): ${violation.help}\n${targets}`;
    })
    .join("\n");
  throw new Error(`Accessibility violations on ${pageFile} at ${viewport.name} width:\n${details}`);
}
