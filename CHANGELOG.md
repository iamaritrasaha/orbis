# Changelog

All notable changes to Orbis are documented here.

## 0.1.0-beta.1.dev.11 (development)

- Refine default update and history output into grouped, human-oriented terminal views with honest coverage and relative timestamps.
- Keep transient update checking inside one persistent command header and preserve full diagnostic records in plain and JSON output.

## 0.1.0-beta.1.dev.3 (development)

- Restore the terminal before administrator authentication and prevent interactive sudo from worker execution paths.
- Replace the dashboard card grid with a selectable command list, real software pulse, and recent history; use terminal dossiers for review and help.
- Separate live provider summaries from scrolling details, animate active providers, and interpret stable APT repository lines for presentation.
- Preserve safe completion on Ctrl+C during provider work and keep upgrade revalidation intact.

## [0.1.0-beta.1] — 2026-09-08

Orbis's first early public release.

### Added

- Linux-first package discovery and safe, provider-specific transaction planning across APT, Flatpak, Snap, Cargo, npm, pnpm, uv, and pipx.
- Read-only updates, coordinated upgrade planning, conservative cleanup planning, transaction history, package explanations, diagnostics, JSON output, and the interactive terminal dashboard.
- Reproducible Linux x86_64 and ARM64 release archives, SHA-256 checksums, a user-owned shell installer, release metadata, and GitHub artifact attestations through `dist`.
- A man page and shell completion files in each release archive.

### Release status

This is a Beta release: safety-first but still evolving. Review every transaction plan before mutation. Orbis does not publish to crates.io in this release.
