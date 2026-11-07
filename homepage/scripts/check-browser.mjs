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
  nav: ".mono-header .mono-menu nav",
  // Documentation pages carry Ardo's drawer; the homepage folds its links into the Monografie menu.
  menu: 'button[aria-label="Toggle menu"]',
  homeMenu: ".mono-header .mono-menu-button",
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

  await verifyHomepage(page, origin);
  await verifyNavigation(page, origin);
  await verifySearch(page, origin);
  await page.emulateMedia({ reducedMotion: "reduce" });
  const motion = await page
    .locator(".mono, .mono *")
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
    "Verified all pages at eight widths, the homepage's real-output toggles, chart and menu, navigation, search, focus return, reduced motion and runtime errors.",
  );
} finally {
  await browser?.close();
  await previewServer?.close();
}

/** The homepage's toggles, the chart's tooltip and the Monografie menu. */
async function verifyHomepage(page, origin) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(new URL("/", origin).href, { waitUntil: "load" });
  await verifyMatcher(page);
  await verifyWalk(page);
  await verifyChart(page);
  await verifyHomepageMenu(page, origin);
}
async function verifyMatcher(page) {
  const section = page.locator("#overview");
  await section.getByText("Five paths, three selected", { exact: true }).waitFor();
  await section.getByRole("button", { name: "*.ts", exact: true }).click();
  await section.getByText("Five paths, one selected", { exact: true }).waitFor();
}
async function verifyWalk(page) {
  const example = page.locator("#example");
  const choices = example.getByRole("group", { name: "Search", exact: true });
  await example.getByText("Three files found", { exact: true }).waitFor();
  assert.match(await example.textContent(), /None\./);
  await choices.getByRole("button", { name: "Whole project", exact: true }).click();
  await example.getByText("Four files found", { exact: true }).waitFor();
  assert.match(await example.textContent(), /public\/legacy\.ts/);
  assert.match(await example.textContent(), /Permission denied/);
  await choices.getByRole("button", { name: "Without ignore rules", exact: true }).click();
  await example.getByText("Five files found", { exact: true }).waitFor();
  assert.match(await example.textContent(), /node_modules\/vendor\/index\.ts/);
  await example.locator("summary").click();
  assert.match(await example.textContent(), /pgdata\/base\.ts/);
  await example.locator("summary").click();
}
async function verifyChart(page) {
  const chart = page.locator("#speed");
  await chart.getByRole("button", { name: "Match one path", exact: true }).click();
  const bar = chart.locator(".mono-bars li").nth(1);
  await bar.focus();
  await chart.locator(".mono-tip[data-show]").waitFor();
  assert.match(await chart.locator(".mono-tip").textContent(), /as long as Ferralk/);
  assert.doesNotMatch(await chart.textContent(), /NaN|Infinity|undefined/);
}
async function verifyHomepageMenu(page, origin) {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(new URL("/", origin).href, { waitUntil: "load" });
  const button = page.locator(selectors.homeMenu);
  await button.click();
  await page.locator(selectors.nav).getByRole("link", { name: "Guide", exact: true }).waitFor();
  await page.keyboard.press("Escape");
  assert.equal(await button.getAttribute("aria-expanded"), "false", "The menu did not close");
  assert(
    await button.evaluate((element) => element === document.activeElement),
    "Menu focus was not restored",
  );
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

  await verifyHeader(page, pageFile, viewport);
  await verifyAccessibility(page, pageFile, viewport);
  console.log(`Verified ${pageFile} at ${viewport.name} (${viewport.width}px).`);
}
/** The display of the first element a selector finds, or "none" when there is none. */
async function display(page, selector) {
  const element = page.locator(selector).first();
  if ((await element.count()) === 0) return "none";
  return element.evaluate((node) => getComputedStyle(node).display);
}
async function verifyHeader(page, pageFile, viewport) {
  // The Monografie header folds its links into a menu below 961px. On the
  // homepage the menu button takes them; documentation pages hide it and keep
  // Ardo's drawer, which also stands in for the guide sidebar while it is hidden.
  assert(
    await page.locator(selectors.search).isVisible(),
    "The site should keep search visible at each checked viewport",
  );
  const wide = viewport.width >= 961;
  const links = await display(page, ".mono-header .mono-menu");
  if (wide) assert.notEqual(links, "none", `${pageFile} hides the header links on wide screens`);
  else assert.equal(links, "none", `${pageFile} shows the header links at ${viewport.name}`);
  const homeMenu = await display(page, selectors.homeMenu);
  if (pageFile === "index.html") {
    assert.equal(homeMenu === "none", wide, `${pageFile} menu button at ${viewport.name}`);
    return;
  }
  assert.equal(homeMenu, "none", `${pageFile} shows a second menu at ${viewport.name}`);
  const drawer = await display(page, selectors.menu);
  const sidebar = await display(page, ".mono-reading-navigation");
  assert(
    drawer !== "none" || sidebar !== "none",
    `${pageFile} offers no documentation navigation at ${viewport.name}`,
  );
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
