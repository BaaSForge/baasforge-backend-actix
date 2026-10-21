# Security Policy

## Scope

This policy covers the BaaSForge backend repository and security issues
that cross the BaaSForge ecosystem:

- [Backend](https://github.com/BaaSForge/baasforge-backend-actix): the main project entry point and control plane.
- [Runtime](https://github.com/BaaSForge/baasforge-runtime-actix): the execution plane.
- [Frontend](https://github.com/BaaSForge/baasforge-frontend-vite): dashboard, public pages, and documentation.

Areas of particular interest in this repository include:

- Authentication and authorization in HTTP APIs, runtime proxying, and WebSocket subscriptions.
- Access to another user's projects, chat sessions, releases, or events.
- Handling of secrets, generated TypeScript units, chat/LLM work, and technical review.
- Trust boundaries between the backend, runtime, and supporting infrastructure.

These examples describe reporting scope, not a security certification or a claim
that a particular vulnerability exists.

## Reporting a vulnerability

Email **[team@baasforge.com](mailto:team@baasforge.com)** privately. Use a subject
such as **BaaSForge security report**. This contact covers all three repositories;
you do not need to disclose the same issue in multiple places.

Alternatively, if GitHub private vulnerability reporting is enabled, use
**Report a vulnerability** from this repository's
[Security Advisories](https://github.com/BaaSForge/baasforge-backend-actix/security/advisories)
page. If the button is unavailable, use the email contact above. See
[GitHub's private reporting instructions](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/report-privately).

Do not disclose exploit details in a public issue, pull request, discussion,
or uncoordinated patch. General bugs and support questions belong in
[GitHub Issues](https://github.com/BaaSForge/baasforge-backend-actix/issues).

## What to include

Provide what you know; a complete exploit is not required to raise a concern.
Useful information includes:

- Affected repositories, commit SHAs or tags, and deployment context.
- A description of the suspected issue and the access or privileges required.
- Minimal reproduction steps, preferably against a local environment you control.
- Expected and observed behavior, and the potential impact on users or data.
- Redacted logs, screenshots, or a small proof of concept, when useful.
- Whether you have observed exploitation, shared the report elsewhere, or found
  the same issue in another BaaSForge component.
- Your preferred contact method and whether you would like public credit.

Never include live tokens, private keys, customer data, or another person's
credentials. Use synthetic data and redact sensitive values. If a credential
has been exposed, its owner should revoke or rotate it rather than rely on
removing the public copy alone.

## Safe investigation

Test only systems, accounts, projects, and data you own or have explicit
permission to assess. Use local deployments and synthetic fixtures where
possible. Do not access other users' data, run destructive tests, disrupt shared
services, send unsolicited messages, or create charges to demonstrate impact.
If you encounter data you are not authorized to access, stop and report the
minimum information needed to explain the issue.

This policy is not authorization to test BaaSForge's hosted services or
third-party infrastructure. Obtain explicit permission before testing them.

## Versions and maintenance

BaaSForge has not published a supported-version matrix, long-term support
schedule, or security backport commitment. Deployment tags do not by themselves
establish a security support window. Do not assume an older tag receives fixes.

Report suspected vulnerabilities in any revision and include its exact commit
or tag. Maintainers will assess the affected revisions during triage. There is
no guaranteed acknowledgement, remediation, or disclosure deadline in this
policy.

## Coordinating a fix and disclosure

The intended process is to review the report privately, clarify reproduction
and impact, identify affected components, and coordinate a fix or mitigation.
For issues spanning repositories, maintainers should coordinate the changes
before public disclosure.

Please agree on disclosure timing with maintainers before publishing details.
Public information should identify affected revisions, available fixes or
mitigations, and any required operator action without exposing secrets or
private user data. Credit should follow the reporter's preference.

This policy does not establish a paid bug bounty or promise a reward.

## Deployment and contribution guidance

Follow the existing setup and production documentation; tracked development
values are not production credentials. Keep secrets and private deployment data
out of source changes, logs, screenshots, and public reports.

Repository-specific guidance: [Architecture](docs/architecture.md), [Configuration](docs/configuration.md), and [Production and self-hosting](docs/production.md).

Security fixes should follow [CONTRIBUTING.md](CONTRIBUTING.md), retain relevant
compatibility requirements, and include appropriate regression verification.
Keep sensitive reproduction details private until disclosure is coordinated.
For harassment or community concerns, use the
[Code of Conduct](CODE_OF_CONDUCT.md).
