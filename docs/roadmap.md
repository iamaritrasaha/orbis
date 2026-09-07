# Orbis roadmap

This roadmap is deliberately staged around safety and user understanding.

## Milestone 1 — foundation and read-only discovery (current)

- Rust workspace and stable CLI foundation.
- APT/Nala detection and read-only APT metadata.
- Flatpak detection and column-based discovery.
- Snap detection and read-only search/info.
- Provider-neutral package models and source-qualified references.
- Search aggregation, ambiguity handling, Orbis Brief explanations, JSON, diagnostics, tests, and CI.

## Milestone 2 — safe package operations

- Typed operation planner.
- Provider-specific install and remove plans.
- Preview/dry-run where the provider supports it.
- Explicit confirmation and clear impact summaries.
- Narrow privilege handling with an audited boundary.
- Operation records for the current invocation.

## Milestone 3 — maintenance and safety intelligence

- Updates and upgrades.
- Cleanup/autoremove planning.
- Package history.
- Reverse-dependency and safety context.
- A trustworthy `why` command for installed software.

## Milestone 4 — additional ecosystems

- Cargo.
- npm and pnpm.
- uv and pipx.
- Additional sources only when their capability and safety models fit the provider contract.

The roadmap does not promise a daemon, hosted backend, telemetry, or AI dependency.
