# Orbis

Your Linux software, in one place.

Orbis is a Linux-first package-management experience built on top of the package managers people already trust. It brings system packages, desktop applications, and user-wide developer tools into one calm, understandable terminal interface without reimplementing dependency resolution or inventing a new package format.

## 0.1.0-beta.1 — Beta

This is Orbis's early public release: safety-first but still evolving. Orbis is Linux-first and currently targets GNU/Linux on x86_64 and ARM64. It is not a stable release. Continue reviewing every Orbis transaction plan before allowing package state to change; `--plan` and `--dry-run` are available whenever you want to inspect a plan without mutation.

## Milestone 4: system and developer-tool coverage — complete

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

## Milestone 5: Signature terminal experience — complete

Orbis now has two complementary modes:

- fast, script-friendly commands such as `orbis search ripgrep`, `orbis updates`, and `orbis upgrade --plan`;
- an interactive dashboard launched by `orbis` in a capable TTY, or explicitly with `orbis dashboard` (the short alias is `orbis ui`).

The dashboard opens with provider state and confirmed updates, then lets you search with `/`, move with arrows or `j`/`k`, inspect an Orbis Brief, review updates, browse sources and history, and read `why` explanations. `r` refreshes read-only local/provider state; it never refreshes package indexes. TUI mutations always return to the same typed plan, confirmation, executor, verification, and history path used by the CLI. `--plain`, `TERM=dumb`, `NO_COLOR`, JSON mode, pipes, and non-interactive input remain safe fallbacks.

Preview at 80 columns:

~~~text
◈ ORBIS
  Your Linux software, in one place.

SYSTEM & DESKTOP             DEVELOPER TOOLS
  ● APT       ready             ● Cargo     ready
  ○ Flatpak   unavailable       ● npm       ready
  ● Snap      ready             ◐ pnpm      restricted
                                 ● uv        ready
                                 ○ pipx      unavailable
Updates   checking…
  r refreshes local/provider state · no catalog mutation
~~~

## Install Beta 1

Orbis 0.1.0-beta.1 is distributed through GitHub Releases. The installer and archives are Linux-only for this beta.

### Recommended: GitHub release installer

Once the release is available, install the matching prebuilt binary with the generated first-party installer:

~~~sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/iamaritrasaha/orbis/releases/download/v0.1.0-beta.1/orbis-installer.sh | sh
~~~

The installer places Orbis in the user-owned `~/.local/bin` directory and does not require `sudo`. It may offer to add that directory to your login path; restarting the shell or sourcing the suggested environment file makes the command available. Installing Orbis is separate from Orbis later requesting administrator authorization for a system package operation.

### Download an archive manually

Choose the archive for your Linux architecture from the [0.1.0-beta.1 release](https://github.com/iamaritrasaha/orbis/releases/tag/v0.1.0-beta.1), download its matching `.sha256` file, and verify it before extracting. For x86_64, the archive is named `orbis-x86_64-unknown-linux-gnu.tar.xz`; ARM64 uses `orbis-aarch64-unknown-linux-gnu.tar.xz`. The archive contains the `orbis` binary, `README.md`, `LICENSE`, a man page, and shell completions.

~~~sh
version=0.1.0-beta.1
target=x86_64-unknown-linux-gnu
base=https://github.com/iamaritrasaha/orbis/releases/download/v${version}
archive=orbis-${target}.tar.xz
curl --proto '=https' --tlsv1.2 -fL -o "${archive}" "${base}/${archive}"
curl --proto '=https' --tlsv1.2 -fL -o "${archive}.sha256" "${base}/${archive}.sha256"
sha256sum --check "${archive}.sha256"
tar -xJf "${archive}"
cd "orbis-${target}"
install -Dm755 orbis "$HOME/.local/bin/orbis"
~~~

### Build and install from source

For developers working from a clone, install the CLI package explicitly from this workspace:

~~~sh
cargo install --path crates/orbis-cli
~~~

Or build without installing it:

Build the binary with stable Rust:

~~~sh
cargo build --release
./target/release/orbis
~~~

Typical commands:

~~~text
orbis
orbis dashboard
orbis --plain
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
  orbis-cli/     cli.rs parsing, commands.rs orchestration, render/ plain output and theme, tui/ dashboard
docs/
  architecture.md
  developer-providers.md
  roadmap.md
  releases.md
  transactions.md
~~~

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for the scoped plan:

1. Foundation and read-only discovery — complete.
2. Safe operation planning, dry-runs, confirmation, privilege handling, and operation records — complete.
3. Updates, upgrades, cleanup, history, safety intelligence, and why — complete.
4. Cargo, npm/pnpm, uv, and pipx — complete.
5. Signature terminal experience — complete.
6. Release maturity — current; see [docs/releases.md](docs/releases.md) for the beta release gate.

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/architecture.md](docs/architecture.md) before changing provider behavior. Keep provider parsing fixture-driven, preserve the read-only boundary, and run the complete local verification suite before opening a pull request.

Orbis is released under the MIT license.
