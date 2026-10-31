# Security policy

## Reporting a vulnerability

Please report security problems **privately**, not in a public issue or pull
request:

1. Open <https://github.com/Raudbjorn/warp-terminal/security/advisories/new>
   (the repository's *Security* tab, then *Report a vulnerability*).
2. Include what is affected, how to reproduce it, and the impact you expect.
   Say which commit or build you tested.

This is a small project and reports are handled on a best-effort basis; there is
no guaranteed response time.

## Scope

In scope: code in this repository, including the fork-specific parts:

- the local-only network policy (`crates/network_policy`, `crates/http_client`)
  and anything that lets a request leave the machine when it should not;
- the local AI client, the OpenCode sidecar, and the bundled local agent
  service;
- extraction and handling of downloaded archives;
- the plugin host.

Some gaps in the local-only policy are already known and tracked in public
issues (for example
[#32](https://github.com/Raudbjorn/warp-terminal/issues/32)). A report that
adds a new bypass or a worse impact is still welcome.

Vulnerabilities that exist in upstream Warp code and are not specific to this
fork should also be reported to the upstream project through its own channels.

## Supported versions

Only the current `master` branch is supported. There are no maintained release
branches.

## Dependencies

Dependency advisories are tracked with GitHub Dependabot alerts on this
repository. If you find a vulnerable dependency that is reachable in a way the
alert does not describe, a private report is appropriate.
