# Security policy

## Reporting a vulnerability

Please report security issues privately through
[GitHub security advisories](https://github.com/andreylukin/where-next/security/advisories/new).
Do not open a public issue for a vulnerability.

We aim to acknowledge reports within 2 business days and to agree on a disclosure timeline with you.

## Scope

In scope:

- The `wn` binary, daemon and MCP server.
- Release artifacts, installers and their verification.
- Handling of indexed content, including files that contain prompt-injection text aimed at agents.
- Anything that could send local code or usage data off the machine.

Out of scope: vulnerabilities in third-party models or agents, unless where-next makes them
exploitable.

## Supported versions

where-next has not had a release yet. Once it does, security fixes go to the latest release.
