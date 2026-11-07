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
import { MarkDefs } from "ferramenta-family";
import {
  FerraGuideNavigation,
  FerraPage,
  FerraReadingLayout,
  FerraReadingTable,
  FerraTabs,
} from "ferramenta-family/ferra";
import touchIcon from "ferramenta-family/ferra-assets/ferralk/apple-touch-icon.png?url";
import favicon from "ferramenta-family/ferra-assets/ferralk/favicon.ico?url";
import { MonoFooter, MonoHeader, MonoPage } from "ferramenta-family/monografie";
import "ferramenta-family/ferra.css";
import "ferramenta-family/monografie.css";

import navigation from "./data/navigation.json";
import site from "./data/site.json";
import "ferramenta-family/ferra-ardo.css";

import "./components/site-shell.css";

export function Layout({ children }: { children: ReactNode }) {
  return (
    <ArdoRootLayout
      colorScheme="light"
      className="ferra-tokens ferra-ardo"
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
    <FerraGuideNavigation aria-label="Documentation pages">
      <NavLink to="/" onClick={onNavigate}>
        Overview
      </NavLink>
      <FerraTabs aria-label="Documentation sections">
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
      </FerraTabs>
      <p>{current.label}</p>
      {current.pages.map(({ title, to: href }) => (
        <NavLink key={href} to={href} onClick={onNavigate}>
          {title}
        </NavLink>
      ))}
    </FerraGuideNavigation>
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
    <FerraReadingLayout
      id="ferralk-article"
      navigation={<GuideNavigation />}
      outline={<ArdoTOC source="dom" scope="#ferralk-article" />}
    >
      <header>
        <p>Ferralk / {current?.label ?? "Documentation"}</p>
        <h1>{page?.title.replaceAll("`", "") ?? "Documentation"}</h1>
      </header>
      {children}
    </FerraReadingLayout>
  );
}
function TaskStatus(props: ComponentPropsWithoutRef<"input">) {
  return <input {...props} disabled aria-label={props.checked ? "Completed task" : "Open task"} />;
}

const components = { table: FerraReadingTable, input: TaskStatus, wrapper: Article };

/**
 * Every page wears the Monografie header and footer. The homepage reads on the
 * Monografie page ground; documentation keeps the Ferra reading layout.
 */
export default function Root() {
  const path = useArdoPath();
  const home = path === "/" || path === "";
  const shell = (
    <>
      <MarkDefs />
      <ArdoRoot
        className="ferra-ardo-shell"
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
    <FerraPage className="ferra-site fk-docs">{shell}</FerraPage>
  );
}
