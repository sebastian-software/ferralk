import type { ComponentPropsWithoutRef, ReactNode } from "ardo/react";

import { ArdoActiveLink as NavLink } from "ardo/runtime";
import {
  MonoArdoErrorBoundary,
  MonoArdoLayout,
  MonoArdoSite,
  type MonoDocsSection,
} from "ferramenta-family/ardo";
import touchIcon from "ferramenta-family/ferra-assets/ferralk/apple-touch-icon.png?url";
import favicon from "ferramenta-family/ferra-assets/ferralk/favicon.ico?url";

import navigation from "./data/navigation.json";
import site from "./data/site.json";
import "ferramenta-family/monografie.css";
import "ferramenta-family/monografie-docs.css";
import "ferramenta-family/monografie-ardo.css";

export function Layout({ children }: { children: ReactNode }) {
  return (
    <MonoArdoLayout icon={favicon} appleTouchIcon={touchIcon}>
      {children}
    </MonoArdoLayout>
  );
}
export const ErrorBoundary = MonoArdoErrorBoundary;

/** A Markdown task-list box: read-only, and named for readers who cannot see it. */
function TaskStatus(props: ComponentPropsWithoutRef<"input">) {
  return <input {...props} disabled aria-label={props.checked ? "Completed task" : "Open task"} />;
}

const sections: MonoDocsSection[] = navigation.map((section) => ({
  id: section.id,
  label: section.label,
  to: section.to,
  pages: section.pages.map(({ title, to }) => ({ label: title, to })),
}));

const links = [
  <NavLink key="guide" to="/guide/getting-started">
    Guide
  </NavLink>,
  <NavLink key="reference" to="/reference/patterns">
    Reference
  </NavLink>,
  <NavLink key="evidence" to="/evidence/benchmarks">
    Evidence
  </NavLink>,
  <a key="api" href="https://docs.rs/ferralk">
    API
  </a>,
  <a key="github" href="https://github.com/sebastian-software/ferralk">
    GitHub
  </a>,
];

/** Every page on Monografie's Ardo shell; the documentation adds its menu, search and reading layout. */
export default function Root() {
  return (
    <MonoArdoSite
      product="ferralk"
      name="Ferralk"
      release={site.version}
      links={links}
      sections={sections}
      components={{ input: TaskStatus }}
      homeClassName="fk-home"
      docsClassName="fk-docs"
      footer={
        <>
          <a href="https://github.com/sebastian-software/ferralk">GitHub</a>
          <a href="https://crates.io/crates/ferralk">crates.io</a>
          <a href="https://docs.rs/ferralk">docs.rs</a>
        </>
      }
    />
  );
}
