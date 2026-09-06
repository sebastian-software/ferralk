# Security policy

## Supported versions

The supported version is the latest published release. Earlier releases are not
patched. A fix that is not published yet is available on the default branch
until the next release carries it.

Update to the latest release before reporting a problem that may already be
fixed.

## Reporting a vulnerability

Report suspected vulnerabilities privately. Two channels are available:

- **GitHub private vulnerability reporting** — open this repository's
  **Security** tab and choose
  [Report a vulnerability](https://github.com/sebastian-software/ferralk/security/advisories/new).
- **Email** — security@sebastian-software.de.

Include the affected version or commit, platform, input or filesystem shape,
impact, and a minimal reproduction when it is safe to share one. Leave out
credentials and data you are not allowed to share.

Do not open a public issue for a vulnerability before a fix and disclosure plan
are agreed. This is especially important for denial-of-service findings in
pattern compilation, matching, directory parsing, or filesystem traversal.

## Response expectations

Reports are investigated privately. The maintainer aims to:

- Acknowledge a private report within 7 days.
- Assess severity and affected versions within 14 days.
- Coordinate a fix and a disclosure timeline with the reporter.

Once a fix is available, the maintainer will coordinate release notes and public
credit with the reporter unless the reporter prefers to remain anonymous. Timing
can vary for low-impact reports and for reports that depend on a fix in an
upstream dependency.
