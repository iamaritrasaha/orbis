# Orbis roadmap

This roadmap is deliberately staged around safety and user understanding.

## Milestone 1 — foundation and read-only discovery (complete)

- Rust workspace and stable CLI foundation.
- APT/Nala detection and read-only APT metadata.
- Flatpak detection and column-based discovery.
- Snap detection and read-only search/info.
- Provider-neutral package models and source-qualified references.
- Search aggregation, ambiguity handling, Orbis Brief explanations, JSON, diagnostics, tests, and CI.

## Milestone 2 — safe package operations (complete)

- Typed operation planner and strict single-package resolution.
- APT install/remove plans using `apt-get -s`.
- Flatpak scoped install/uninstall plans with honest partial impact.
- Snap install/remove plans with explicit channel and retained-data semantics.
- `--plan`/`--dry-run`, explicit confirmation, `--yes`, and noninteractive refusal.
- Narrow privilege handling with typed provider operations.
- Post-operation verification and sanitized XDG transaction records.

Not included in this milestone: upgrades, update-all, autoremove, cleanup, rollback, batch operations, or dependency-history intelligence.

## Milestone 3 — maintenance and safety intelligence (complete)

- Updates and upgrades.
- Cleanup/autoremove planning.
- Package history.
- Reverse-dependency and safety context.
- A trustworthy `why` command for installed software.

Milestone 3 establishes update versus updates versus upgrade semantics, provider-scoped maintenance, non-atomic unified results, revalidation, APT autoremove planning, history queries, and truthful provider limitations.

## Milestone 4 — additional ecosystems (complete)

- Cargo.
- npm and pnpm.
- uv and pipx.
- Additional sources only when their capability and safety models fit the provider contract.

Milestone 4 manages user-wide developer tools only. It does not manage project dependency graphs, package manifests, lockfiles, virtual environments, or caches. Cargo automatic updates remain incomplete when original install provenance cannot be proven safely. npm excludes suspicious newer-than-latest installs, uv preserves recorded constraints/settings, and pipx preserves pins.

The roadmap does not promise a daemon, hosted backend, telemetry, or AI dependency.

## Milestone 5 — Signature terminal experience (complete)

- Interactive Ratatui dashboard with read-only background loading.
- Search, Orbis Brief package detail, unified updates, sources, history, why, help, resize, and safe plan review screens.
- Centralized terminal theme with color, `NO_COLOR`, monochrome, and ASCII fallbacks.
- Extracted CLI parsing, command orchestration, plain rendering, and TUI presentation boundaries.
- Deterministic plain and in-memory TUI rendering tests.

This milestone does not add package ecosystems or a background service. The ordinary CLI and JSON contracts remain the automation surface.

## Milestone 6 — release maturity (current)

The first beta release gate covers the release identity, reproducible Linux artifacts, user-owned installation, checksums, provenance, package/licensing metadata, release documentation, and CI validation. New provider breadth is not part of this milestone.

The repository is release-ready for `v0.1.0-beta.1` once the complete local and CI validation suite passes. Publishing the version tag and GitHub Release remains an explicit final gate after those checks.
