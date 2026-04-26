# Security Policy

## Supported versions

Gossamer is pre-1.0 and ships from `main`. Tagged releases older
than the most recent tag are unsupported.

## Reporting a vulnerability

Please report suspected vulnerabilities privately rather than in
public issues or pull requests.

- Open a private security advisory via GitHub:
  `https://github.com/gossamer-lang/gossamer/security/advisories/new`.
- If that channel is unavailable, email the maintainers listed in
  `Cargo.toml` under `authors`.

Please include:

- A description of the issue and its impact.
- A minimal reproducer (input file, command line, or curl request).
- The affected commit hash or tag.
- Any suggested mitigation or patch, if you have one.

Do not file public issues, pull requests, or discussion posts for
unfixed vulnerabilities.

## What we consider in scope

- Memory safety or panic-from-untrusted-input in the compiler front
  end (lexer, parser, resolver, type checker, HIR lowering).
- Memory safety or panic-from-untrusted-input in the HTTP server
  (`std::http::server`) and HTTP client.
- Dependency-resolution or manifest-parser issues that let a
  malicious package compromise `gos build` or `gos tidy`.
- Code-execution issues in `gos run` / `gos build` on attacker-
  controlled source files.
- Launcher-script injection via crafted paths or file names.

## Out of scope

- Self-DoS: slow programs, large files, or runaway recursion in the
  interpreter. These are bugs, not vulnerabilities.
- Outputs that depend on intentionally-disabled lints (for example,
  suppressing `unused_variable` with `_` prefixes).
- Vulnerabilities requiring a pre-existing local root compromise.
