import { defineArdoConfig } from "ardo/config";

export default defineArdoConfig({
  title: "Ferralk",
  description:
    "Glob matching and parallel file discovery in Rust. Prune from your patterns, preserve native filenames and choose traversal policy explicitly.",
  siteUrl: "https://ferralk.dev",
  icons: false,
  markdown: { headingAttributes: true },
  linkCheck: { level: "error" },
});
