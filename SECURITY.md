# Security Policy

## Reporting a Vulnerability

**Do not open a public issue.** Instead, use [GitHub private vulnerability reporting](https://github.com/calebfaruki/sycophant/security/advisories/new) to submit your report.

Include: what you found, steps to reproduce, and which version you tested against.

## Response

You should receive an acknowledgment within 48 hours. Security fixes are prioritized over all other work. We aim to release a fix within 90 days of a confirmed report, coordinating public disclosure timing with the reporter.

## Supported Versions

Only the latest release receives security patches.

## Scope

In scope is any way a fully compromised workspace could: read a secret, reach a network destination its chamber didn't declare, forge or rewrite its conversation log, impersonate another workspace or tenant, escape its sandbox, or tamper with the policy machinery that enforces those boundaries. See [THREAT_MODEL.md](THREAT_MODEL.md) for the full model.

Out of scope: denial of service and resource exhaustion, operator-level cluster access (a cluster administrator is trusted), and the behavior of operator-chosen chamber images.
