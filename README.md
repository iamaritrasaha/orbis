# Orbis

Your Linux software, in one place.

Orbis is a Linux-first package-management experience built on top of the package managers people already trust. It brings APT, Flatpak, and Snap discovery into one calm, understandable terminal interface without reimplementing dependency resolution or inventing a new package format.

## Milestone 2: careful single-package operations

The current milestone keeps discovery and mutation as separate paths. Orbis can:

- detect APT/Nala, Flatpak, and Snap;
- search available package sources and normalize the results;
- show package metadata and installed state;
- explain packages through deterministic, evidence-aware Orbis Briefs;
- run safe diagnostics; and
- emit JSON for scripts and automation;
- build provider-specific install/remove plans; and
- execute one exact install or remove only after a clear confirmation boundary.

Milestone 2 does not implement upgrades, refreshes, autoremove, cleanup, rollback, batch operations, or package-manager index changes as part of planning. Flatpak and Snap plans are explicitly marked partial where their CLIs do not provide an APT-style no-action simulation.

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
orbis explain ffmpeg
orbis doctor
orbis --json search btop
orbis install btop --source apt --plan
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

| Source | Discovery | Milestone 2 operations |
| --- | --- | --- |
| APT | Supported | apt-get -s plan, exact apt-get install/remove |
| Flatpak | Supported when installed | scoped install/uninstall; partial remote metadata plan |
| Snap | Supported when installed | install/remove; partial store metadata plan |

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
  roadmap.md
  transactions.md
~~~

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for the scoped plan:

1. Foundation and read-only discovery — complete.
2. Safe operation planning, dry-runs, confirmation, privilege handling, and operation records — current.
3. Updates, upgrades, cleanup, history, safety intelligence, and 'why'.
4. Cargo, npm/pnpm, uv, and pipx.

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/architecture.md](docs/architecture.md) before changing provider behavior. Keep provider parsing fixture-driven, preserve the read-only boundary, and run the complete local verification suite before opening a pull request.

Orbis is released under the MIT license.
