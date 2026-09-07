# Changelog

All notable changes to Orbis are documented here.

## [0.1.0-beta.1] — 2026-09-08

Orbis's first early public release.

### Added

- Linux-first package discovery and safe, provider-specific transaction planning across APT, Flatpak, Snap, Cargo, npm, pnpm, uv, and pipx.
- Read-only updates, coordinated upgrade planning, conservative cleanup planning, transaction history, package explanations, diagnostics, JSON output, and the interactive terminal dashboard.
- Reproducible Linux x86_64 and ARM64 release archives, SHA-256 checksums, a user-owned shell installer, release metadata, and GitHub artifact attestations through `dist`.
- A man page and shell completion files in each release archive.

### Release status

This is a Beta release: safety-first but still evolving. Review every transaction plan before mutation. Orbis does not publish to crates.io in this release.
