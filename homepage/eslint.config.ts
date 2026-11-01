import { getEslintConfig } from "eslint-config-setup";

const config = await getEslintConfig({ node: true, react: true, oxlint: true });

config.unshift({
  ignores: [
    "**/dist/**",
    "coverage/**",
    "build/**",
    "node_modules/**",
    "pnpm-lock.yaml",
    "**/*.json",
    "**/*.md",
    // Code blocks inside MDX pages are documentation snippets, not project
    // sources: they have no tsconfig entry for the type-aware rules to use.
    "**/*.mdx/**",
  ],
});

export default config;
