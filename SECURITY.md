# Security policy

## Supported versions

Only the latest release on the default branch receives security fixes.

## Reporting a vulnerability

Please **do not** open a public issue for a suspected vulnerability.
Report it privately through GitHub's advisory form:

https://github.com/hellpuffyt/logtail-rs/security/advisories/new

Include the version or commit, a description of the issue, and steps or
input that reproduce it. You should receive an acknowledgement within a
few days; once a fix is available it will be released and credited in
the advisory unless you prefer otherwise.

## Scope

`logtail` reads untrusted log input. Relevant reports include panics, unbounded memory or CPU use driven by input or queries, and anything that makes it read or write files other than the one it was pointed at. It makes no network connections.
