# Orbis

Your Linux software, in one place.

Orbis is a Linux-first package-management experience built on top of the package managers people already trust. It brings APT, Flatpak, and Snap discovery into one calm, understandable terminal interface without reimplementing dependency resolution or inventing a new package format.

## Milestone 1: read-only foundation

The current milestone is deliberately safe. Orbis can:

- detect APT/Nala, Flatpak, and Snap;
- search available package sources and normalize the results;
- show package metadata and installed state;
- explain packages through deterministic, evidence-aware Orbis Briefs;
- run safe diagnostics; and
- emit JSON for scripts and automation.

Orbis does not install, remove, upgrade, refresh, autoremove, clean, or otherwise mutate package state yet. No command in this milestone requires privilege escalation.

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
~~~

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

## Supported sources

| Source | Milestone 1 status | Read-only backend |
| --- | --- | --- |
| APT | Supported | 'apt-cache' and 'dpkg-query'; Nala is detected as an optional frontend |
| Flatpak | Supported when installed | Flatpak column-based output |
| Snap | Supported when installed | 'snap find', 'snap list', and 'snap info' |

Nala is presented as an APT frontend, not as a separate package ecosystem. Orbis owns its own terminal presentation rather than scraping Nala's interactive output.

## Safety boundary

Milestone 1 is read-only. Providers are given explicit read methods and report capabilities rather than being forced to pretend every provider supports every operation. Process execution uses structured arguments and never invokes a shell for package queries. Tests use mocked command runners and do not require sudo or a package-manager database.

The future mutation architecture is documented before it is implemented: operation planning, dry-runs, explicit confirmation, privilege boundaries, and operation records will be added only after the read-only foundation is proven.

## Building and testing

~~~sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
~~~

The test suite uses provider fixtures and an injectable process runner. It does not install packages, remove packages, refresh Snap, change Flatpak remotes, or require a graphical desktop.

## Project layout

~~~text
crates/
  orbis-core/    models, process runner, providers, discovery, explanations, diagnostics
  orbis-cli/     clap command language and terminal presentation
docs/
  architecture.md
  roadmap.md
~~~

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for the scoped plan:

1. Foundation and read-only discovery — current.
2. Safe operation planning, dry-runs, confirmation, privilege handling, and operation records.
3. Updates, upgrades, cleanup, history, safety intelligence, and 'why'.
4. Cargo, npm/pnpm, uv, and pipx.

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/architecture.md](docs/architecture.md) before changing provider behavior. Keep provider parsing fixture-driven, preserve the read-only boundary, and run the complete local verification suite before opening a pull request.

Orbis is released under the MIT license.
