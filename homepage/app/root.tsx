import { type ComponentPropsWithoutRef, type ReactNode, useState } from "ardo/react";
import { ArdoActiveLink as NavLink, useArdoPageData, useArdoPath } from "ardo/runtime";
import {
  ArdoErrorBoundary,
  ArdoHeaderSearch,
  ArdoMobileNavigation,
  ArdoRoot,
  ArdoRootLayout,
  ArdoTOC,
} from "ardo/ui";
import touchIcon from "ferramenta-family/ferra-assets/ferralk/apple-touch-icon.png?url";
import favicon from "ferramenta-family/ferra-assets/ferralk/favicon.ico?url";
import {
  MarkDefs,
  MonoFooter,
  MonoGuideNavigation,
  MonoHeader,
  MonoPage,
  MonoReadingLayout,
  MonoReadingTable,
  MonoSectionRail,
} from "ferramenta-family/monografie";
import "ferramenta-family/monografie.css";
import "ferramenta-family/monografie-docs.css";
import "ferramenta-family/monografie-ardo.css";

import navigation from "./data/navigation.json";
import site from "./data/site.json";

export function Layout({ children }: { children: ReactNode }) {
  return (
    <ArdoRootLayout
      className="mono-ardo"
      colorScheme="light"
      icon={favicon}
      appleTouchIcon={touchIcon}
      iconBasePath={false}
    >
      {children}
    </ArdoRootLayout>
  );
}
export const ErrorBoundary = ArdoErrorBoundary;

function GuideNavigation({ onNavigate }: { onNavigate?: () => void }) {
  const path = useArdoPath();
  const first = navigation[0];
  if (!first) throw new Error("Documentation navigation is empty.");
  const current =
    navigation.find((section) => path === `/${section.id}` || path.startsWith(`/${section.id}/`)) ??
    first;
  return (
    <>
      <MonoSectionRail aria-label="Documentation sections">
        {navigation.map((section) => (
          <NavLink
            key={section.id}
            to={section.to}
            data-active={current.id === section.id}
            onClick={onNavigate}
          >
            {section.label}
          </NavLink>
        ))}
      </MonoSectionRail>
      <MonoGuideNavigation aria-label="Documentation pages">
        <NavLink to="/" onClick={onNavigate}>
          Overview
        </NavLink>
        <p>{current.label}</p>
        {current.pages.map(({ title, to: href }) => (
          <NavLink key={href} to={href} end onClick={onNavigate}>
            {title}
          </NavLink>
        ))}
      </MonoGuideNavigation>
    </>
  );
}

/** The documentation menu and search, which stay in the header on documentation pages. */
function DocsActions() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <ArdoMobileNavigation title="Ferralk documentation" open={open} onOpenChange={setOpen}>
        <GuideNavigation
          onNavigate={() => {
            setOpen(false);
          }}
        />
      </ArdoMobileNavigation>
      <ArdoHeaderSearch
        variant="icon"
        onActivate={() => {
          setOpen(false);
        }}
      />
    </>
  );
}

function Header({ docs }: { docs: boolean }) {
  return (
    <MonoHeader
      product="ferralk"
      home="/"
      release={site.version}
      links={[
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
      ]}
      actions={docs ? <DocsActions /> : <ArdoHeaderSearch variant="icon" />}
    />
  );
}

function Footer() {
  return (
    <MonoFooter product="ferralk" home="/">
      <a href="https://github.com/sebastian-software/ferralk">GitHub</a>
      <a href="https://crates.io/crates/ferralk">crates.io</a>
      <a href="https://docs.rs/ferralk">docs.rs</a>
    </MonoFooter>
  );
}

function Article({ children }: { children: ReactNode }) {
  const page = useArdoPageData();
  const path = useArdoPath();
  const current = navigation.find(
    (section) => path === `/${section.id}` || path.startsWith(`/${section.id}/`),
  );
  return (
    <MonoReadingLayout
      id="ferralk-article"
      navigation={<GuideNavigation />}
      outline={<ArdoTOC source="dom" scope="#ferralk-article" />}
    >
      <header>
        <p>Ferralk / {current?.label ?? "Documentation"}</p>
        <h1>{page?.title.replaceAll("`", "") ?? "Documentation"}</h1>
      </header>
      {children}
    </MonoReadingLayout>
  );
}
function TaskStatus(props: ComponentPropsWithoutRef<"input">) {
  return <input {...props} disabled aria-label={props.checked ? "Completed task" : "Open task"} />;
}

const components = { table: MonoReadingTable, input: TaskStatus, wrapper: Article };

/**
 * Every page is a Monografie page: the homepage, and the documentation in the
 * Monografie reading layout.
 */
export default function Root() {
  const path = useArdoPath();
  const home = path === "/" || path === "";
  const shell = (
    <>
      <MarkDefs />
      <ArdoRoot
        className="mono-ardo-shell"
        scrollMode="document"
        header={<Header docs={!home} />}
        sidebar={false}
        footer={false}
        components={components}
      />
      <Footer />
    </>
  );
  return home ? (
    <MonoPage className="fk-home">{shell}</MonoPage>
  ) : (
    <MonoPage className="mono-docs fk-docs">{shell}</MonoPage>
  );
}
