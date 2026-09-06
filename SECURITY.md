# Security policy

## Supported versions

The supported version is the latest published release. Earlier releases are not
patched. A fix that is not published yet is available on the default branch
until the next release carries it.

Update to the latest release before reporting a problem that may already be
fixed.

## Reporting a vulnerability

Report suspected vulnerabilities privately. Two private channels are available:

- **GitHub private vulnerability reporting** — open this repository's
  **Security** tab and choose **Report a vulnerability**.
- **Email** — security@sebastian-software.de.

Include the affected version or commit, platform, input or filesystem shape,
impact, and a minimal reproduction when it is safe to share one.

Do not open a public issue for a vulnerability before a fix and disclosure plan
are agreed. This is especially important for denial-of-service findings in
pattern compilation, matching, directory parsing, or filesystem traversal.

Maintainers aim to acknowledge private reports within 7 days and assess
severity and affected versions within 14 days. Once a fix is available, the
maintainer will coordinate release notes and public credit with the reporter
unless the reporter prefers to remain anonymous.
