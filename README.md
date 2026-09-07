# Orbis

Your Linux software, in one place.

Orbis is a Linux-first package-management experience built on top of the package managers people already trust. It brings system packages, desktop applications, and user-wide developer tools into one calm, understandable terminal interface without reimplementing dependency resolution or inventing a new package format.

## Milestone 4: system and developer-tool coverage

The current milestone keeps discovery and mutation as separate paths. Orbis can:

- detect APT/Nala, Flatpak, Snap, Cargo, npm, pnpm, uv tools, and pipx;
- search available package sources and normalize the results;
- show package metadata and installed state;
- explain packages through deterministic, evidence-aware Orbis Briefs;
- run safe diagnostics; and
- emit JSON for scripts and automation;
- build provider-specific install/remove plans;
- inspect unified updates across APT, Flatpak, and Snap;
- plan safe coordinated upgrades and conservative cleanup; and
- read sanitized transaction history and evidence-based package explanations.

Developer providers manage user-wide tools only: Cargo binary crates, global npm/pnpm packages, `uv tool` environments, and pipx applications. They never edit project manifests, lockfiles, `node_modules`, `.venv`, or arbitrary Python environments. See [docs/developer-providers.md](docs/developer-providers.md).

See docs/maintenance.md for the exact meaning of update, updates, upgrade, clean, history, and why.

## Try it

Build the binary with stable Rust:

~~~sh
cargo build --release
./target/release/orbis
~~~

Typical commands:

~~~text
orbis
orbis sources
orbis search btop
orbis search btop --source apt
orbis info apt:libssl-dev
orbis info cargo:ripgrep
orbis info npm:typescript
orbis explain ffmpeg
orbis doctor
orbis update --plan
orbis updates
orbis upgrade --plan
orbis clean --plan
orbis history
orbis why apt:libssl3
orbis --json search btop
orbis install btop --source apt --plan
orbis install cargo:ripgrep --plan
orbis install npm:typescript --plan
orbis install pnpm:typescript --plan
orbis install uv:ruff --plan
orbis install pipx:black --plan
orbis remove snap:btop --plan
orbis --json install apt:btop --dry-run
~~~

Use --plan or --dry-run to inspect an operation without changing package state. Without --yes, an actual operation requires an interactive terminal and an explicit prompt. --yes skips only Orbis's prompt after the exact plan has been resolved; it does not bypass provider safeguards or administrator authorization.

On a system without Flatpak or Snap, Orbis keeps working through the providers that are present and explains which sources are unavailable.

Example shape:

~~~text
Orbis
Your Linux software, in one place.

Sources
  ● APT       ready
  ○ Flatpak   unavailable
  ● Snap      ready
  ● Cargo     ready
  ● npm       ready
  ● pnpm      restricted
  ● uv        ready
  ○ pipx      unavailable

Try
  orbis search <package>
  orbis explain <package>
  orbis doctor
~~~

## Why Orbis exists

Linux software is wonderfully diverse, but package ecosystems expose different names, metadata, commands, and assumptions. Orbis is a normalization and explanation layer: the underlying providers remain responsible for their own package databases and future operations.

The long-term goal is to make questions such as these easy to answer:

- What is this package actually for?
- Is it an application, CLI tool, library, runtime, or development component?
- Where did it come from?
- Is it already installed?
- Which source has it, and why might one source be more appropriate?
- Could removing it affect other software?

When Orbis cannot establish an answer, it represents the field as unknown or says that the explanation is based only on provider metadata. It does not call a hosted AI service and does not fabricate package descriptions.

## Supported sources and operations

| Source | Discovery | Maintenance and operations |
| --- | --- | --- |
| APT | Supported | local-index updates, safe apt-get upgrade, autoremove plan, exact install/remove |
| Flatpak | Supported when installed | scoped AppStream refresh, update inventory, conservative cleanup refusal, install/uninstall |
| Snap | Supported when installed | pending refresh inventory, exact-name refresh, snapd-managed cleanup diagnostics, install/remove |
| Cargo | Supported when installed | user binary-crate install/remove, live search/info; upgrade provenance is intentionally incomplete |
| npm | Supported when installed | global JSON discovery/info/outdated, global dry-run planning, exact reviewed upgrades |
| pnpm | Supported when installed and configured | global JSON discovery/info/outdated, exact global operations; no authoritative global dry run |
| uv | Supported when installed | installed `uv tool` state, constrained outdated/upgrade, explicit install/remove; no fuzzy search |
| pipx | Supported when installed | user JSON snapshot, pinned-aware upgrades, exact install/remove; no fuzzy search |

Nala is presented as an APT frontend, not as a separate package ecosystem. Orbis owns its own terminal presentation rather than scraping Nala's interactive output.

## Safety boundary

Providers retain a read-only Provider interface and opt into the separate TransactionProvider interface. A transaction resolves one exact provider before any execution, creates a typed plan, rejects ambiguity and incomplete safety conditions, and shows the target, scope, changes, risk, privilege requirement, and provider limitations.

APT planning uses apt-get -s with a controlled per-command environment and blocks plans that report additional removals. Flatpak never uses --no-deploy for planning, and Snap never uses --purge for removal. System operations cross a narrow typed privilege boundary; Orbis does not expose a generic root command runner and does not handle passwords.

Process execution uses structured arguments and never invokes a shell. Tests use mocked command runners and fake operation executors; they do not require sudo or alter a package-manager database. Every attempted execution is recorded under the XDG state directory without command output, credentials, or tokens.

## Building and testing

~~~sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
~~~

The test suite uses provider fixtures and an injectable process runner. It does not install packages, remove packages, refresh Snap, change Flatpak remotes, or require a graphical desktop.

## Project layout

~~~text
crates/
  orbis-core/    models, process runner, providers, transactions, privilege, explanations, diagnostics
  orbis-cli/     clap command language and terminal presentation
docs/
  architecture.md
  developer-providers.md
  roadmap.md
  transactions.md
~~~

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for the scoped plan:

1. Foundation and read-only discovery — complete.
2. Safe operation planning, dry-runs, confirmation, privilege handling, and operation records — complete.
3. Updates, upgrades, cleanup, history, safety intelligence, and why — complete.
4. Cargo, npm/pnpm, uv, and pipx — current.

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/architecture.md](docs/architecture.md) before changing provider behavior. Keep provider parsing fixture-driven, preserve the read-only boundary, and run the complete local verification suite before opening a pull request.

Orbis is released under the MIT license.
