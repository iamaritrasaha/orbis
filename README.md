<div align="center">

<pre>
╭──╮  ╭──╮  ╭──╮    ╷    ╭──╮
│  │  ├──╯  ├──┤    │    ╰──╮
╰──╯  ╵  ╲  ╰──╯    ╵    ╰──╯

HRIK
</pre>

# Orbis

### Your Linux software, in one place.

A calm, terminal-native way to **find, understand, update, and safely manage software on Linux** — across the package systems you already use.

[![CI](https://github.com/iamaritrasaha/orbis/actions/workflows/ci.yml/badge.svg)](https://github.com/iamaritrasaha/orbis/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/iamaritrasaha/orbis?display_name=tag&sort=semver)](https://github.com/iamaritrasaha/orbis/releases)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)
[![Linux](https://img.shields.io/badge/platform-Linux-black.svg)](#installation)

[Install](#installation) · [Quick start](#quick-start) · [How it works](#how-orbis-works) · [Safety](#safety-by-design) · [Development](#development)

</div>

---

## Why Orbis?

Linux software is powerful, but the experience is fragmented.

A single machine may use **APT**, **Flatpak**, **Snap**, **Cargo**, **npm**, **pnpm**, **uv**, and **pipx** — each with different commands, output, terminology, privilege rules, and update behavior.

Orbis gives those systems one consistent human-facing interface while keeping the native package managers underneath.

```text
You ask:                Orbis figures out:

orbis find firefox      where it is available
orbis show btop         what it is and where it came from
orbis update            what can be updated
orbis install btop      what will change before anything runs
orbis history           what Orbis changed previously
```

Orbis is **not** a new package format, repository, dependency resolver, or replacement for your distribution package manager.

It is a safer, clearer orchestration layer over the software systems already trusted by your machine.

---

## Quick start

```bash
orbis find firefox
orbis show btop
orbis install btop
orbis update
orbis refresh
orbis clean
orbis history
orbis health
```

Run `orbis` by itself for the lightweight task launcher.

Run `orbis ui` for the optional persistent full-screen interface.

### The normal terminal experience

Orbis commands stay in your terminal, preserve scrollback, and return naturally to your shell.

```text
◈ ORBIS // UPDATES
────────────────────────────────────────────────────────────────

5 updates available

SNAP
  code                   a44adf7f → 88e44fa0

NODE.JS
  @deepseek-ai/dsh     0.1.1-rc.2 → 0.1.2-rc.1
  @qwen-code/qwen-code     0.22.2 → 0.23.2
  corepack                    0.35.0 → 0.36.0
  npm                         11.19.0 → 12.0.2

Coverage
  ◇ Ubuntu repositories    status incomplete
  ◇ Rust tools             status incomplete
  ◇ Optional tools         pnpm, pipx unavailable

Read-only · nothing changed
Next  orbis update --apply
```

Animations are short and skippable. `REDUCE_MOTION`, `NO_COLOR`, `TERM=dumb`, `--plain`, pipes, redirected output, and JSON mode all fall back to deterministic non-animated output.

---

## One interface, many software systems

| Orbis view | Native software system | Typical scope |
| --- | --- | --- |
| **Ubuntu repositories** | APT / Nala-compatible Debian packaging | System |
| **Flatpak** | Flatpak | System or user |
| **Snap Store** | Snap | System |
| **Rust tools** | Cargo | User-wide |
| **Node.js tools** | npm / pnpm | User/global tooling |
| **Python tools** | uv tools / pipx | User-wide tooling |

Orbis keeps provider provenance available whenever it matters.

You can always be explicit:

```bash
orbis show apt:btop
orbis install cargo:ripgrep
orbis info flatpak:org.gimp.GIMP
```

If the same software name exists in multiple ecosystems, Orbis does **not** silently choose one for you.

---

## What Orbis does

### Find software

```bash
orbis find btop
```

Search across supported software systems without memorizing each backend command.

### Understand software

```bash
orbis show btop
```

Orbis explains:

- what the software does;
- why it may be useful;
- whether it is installed;
- which source provides it;
- what Orbis knows with confidence;
- what remains unknown.

When Orbis cannot establish a fact reliably, it says so instead of inventing an answer.

### Check updates

```bash
orbis update
```

This is read-only. It checks supported sources and groups the result by software family.

No package state or software catalog is changed.

### Review and apply updates

```bash
orbis update --apply
```

Orbis builds the latest executable plan, shows only actionable changes, separates **coverage limitations** from **execution risk**, and asks for confirmation before mutation.

```text
◈ ORBIS // UPDATE
────────────────────────────────────────────────────────────────

5 updates ready

SNAP · admin
  code                   a44adf7f → 88e44fa0

NODE.JS
  @deepseek-ai/dsh     0.1.1-rc.2 → 0.1.2-rc.1
  @qwen-code/qwen-code     0.22.2 → 0.23.2
  corepack                    0.35.0 → 0.36.0
  npm                         11.19.0 → 12.0.2

Coverage
  ◇ Ubuntu repositories    status incomplete
  ◇ Rust tools             update unavailable
  ◇ Optional tools         pnpm, pipx unavailable

Risk  normal
Only the updates listed above will be changed.

Continue? [Y/n]
```

Genuinely high-impact plans require literal `YES`. A blocked plan cannot cross the execution boundary.

### Refresh software information

```bash
orbis refresh
```

Refreshes only the sources that actually require refresh work. Sources managed automatically or on demand remain visible without pretending to perform work.

### Keep a useful history

```bash
orbis history
```

Human output is intentionally quiet:

```text
◈ ORBIS // HISTORY
────────────────────────────────────────────────────────────────

●  4m ago      Refresh software information
               completed

◐  2d ago      Refresh software information
               completed with limitations
               Cargo upgrade provenance is incomplete

●  3d ago      Install flatpak · Ubuntu repositories
               completed
```

Full operation IDs and structured records remain available in plain, JSON, and detail views.

---

## How Orbis works

Orbis follows a simple boundary:

```text
              human intent
                   │
                   ▼
            ┌─────────────┐
            │    Orbis    │
            │  CLI / UI   │
            └──────┬──────┘
                   │
          resolve + inspect
                   │
                   ▼
            typed operation plan
                   │
            review / confirm
                   │
                   ▼
       narrow provider execution
                   │
       ┌───────────┼───────────┐
       ▼           ▼           ▼
      APT       Flatpak      Snap       ...
       │           │           │
       └───────────┴───────────┘
                   │
             verification
                   │
                   ▼
                history
```

The native package managers remain the source of truth for their ecosystems.

Orbis adds a consistent planning, explanation, confirmation, progress, verification, and history layer around them.

---

## Safety by design

Orbis is conservative on purpose.

- **No shell execution path** — provider commands remain structured arguments.
- **Plan before mutation** — intended changes are resolved before execution.
- **Ambiguity is stopped** — Orbis never silently chooses between conflicting package sources.
- **Privilege is narrow** — administrator access is requested only when the selected operation requires it.
- **No fake dry-runs** — Orbis does not claim certainty when a provider cannot supply it.
- **Blocked means blocked** — non-executable plans cannot be confirmed into execution.
- **Risk and coverage are separate** — an unavailable optional provider does not make unrelated normal updates “high impact”.
- **Developer ecosystems stay global/user-wide** — Orbis does not rewrite project manifests, lockfiles, `node_modules`, `.venv`, or arbitrary Python environments.
- **History is sanitized** — credentials, tokens, and raw command output are not persisted.
- **No telemetry or hosted service** — normal Orbis operation is local and deterministic.

`--yes` skips only the interactive confirmation for an already reviewed executable plan. It does not bypass provider safeguards, plan validation, or administrator authorization.

---

## Installation

### Install the current public Beta

The public release is **v0.1.0-beta.1**.

It installs into the user-owned `~/.local/bin` directory and does not require `sudo`.

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/iamaritrasaha/orbis/releases/download/v0.1.0-beta.1/orbis-installer.sh | sh
```

Then verify:

```bash
orbis --version
orbis health
```

> `main` contains newer development snapshots and may report a `.dev.*` version until the next public release is published.

### Manual archive

```bash
version=0.1.0-beta.1
target=x86_64-unknown-linux-gnu
base=https://github.com/iamaritrasaha/orbis/releases/download/v${version}
archive=orbis-${target}.tar.xz

curl --proto '=https' --tlsv1.2 -fL -o "${archive}" "${base}/${archive}"
curl --proto '=https' --tlsv1.2 -fL -o "${archive}.sha256" "${base}/${archive}.sha256"
sha256sum --check "${archive}.sha256"
tar -xJf "${archive}"
install -Dm755 "orbis-${target}/orbis" "$HOME/.local/bin/orbis"
```

Use `aarch64-unknown-linux-gnu` for ARM64.

### Build from source

Requires stable Rust and the locked workspace dependencies.

```bash
git clone https://github.com/iamaritrasaha/orbis.git
cd orbis
cargo install --path crates/orbis-cli --locked
```

Or run without installing:

```bash
cargo build --workspace --release
./target/release/orbis
```

---

## Updating Orbis itself

A user-local release installation can safely check the official GitHub Releases channel:

```bash
orbis self-update --check
orbis self-update
```

The updater:

1. selects an official Orbis release;
2. asks before downloading;
3. verifies the SHA-256 checksum;
4. validates the archive layout;
5. atomically replaces only the executable that is currently running.

It does not use `sudo`, does not execute downloaded installer scripts, and does not update in the background.

Development builds containing `.dev.` never replace themselves through the release updater.

---

## CLI map

### Everyday commands

```text
find         Find software
show         Understand software
install      Install software
remove       Remove software
update       Check for updates
refresh      Refresh software information
clean        Remove unused software conservatively
history      Review previous Orbis actions
health       Check provider readiness
self-update  Update Orbis itself safely
```

### Advanced / compatibility commands

```text
sources      Inspect provider capabilities and provenance
why          Explain why an installed package is present
ui           Open the persistent full-screen interface
dashboard    Alias for the full-screen interface
search       Compatibility alias for find
info         Provider-oriented package metadata
explain      Compatibility explanation command
upgrade      Advanced compatibility update command
```

Use `orbis --help` or `orbis <command> --help` for the exact current options.

---

## Human, plain, and JSON output

Orbis intentionally has three presentation modes.

### Human mode

Designed for interactive terminals:

- grouped provider families;
- compact explanations;
- transient progress;
- semantic color;
- short animation;
- friendly status language.

### Plain mode

```bash
orbis --plain update
```

Deterministic, sequential, ANSI-free output with more implementation detail.

### JSON mode

```bash
orbis --json search btop
```

Pure structured data for scripts and integrations. No branding, animation, or human reinterpretation is injected into JSON.

---

## Project structure

```text
orbis/
├── crates/
│   ├── orbis-core/       models, providers, plans, safety, execution, history
│   └── orbis-cli/        commands, rendering, transient UI, full-screen TUI
├── docs/                 architecture, maintenance, transactions, releases
├── .github/workflows/    CI and release automation
├── CONTRIBUTING.md
├── CHANGELOG.md
└── README.md
```

The architecture is intentionally split so rendering cannot become an arbitrary privileged execution path.

---

## Development

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
git diff --check
```

Tests use fixtures, mocked runners, fake executors, terminal screen reconstruction, and pseudo-TTY coverage. They do not install or remove real software.

Useful project docs:

- [Architecture](docs/architecture.md)
- [Maintenance model](docs/maintenance.md)
- [Contributing](CONTRIBUTING.md)
- [Release history](docs/releases.md)
- [Changelog](CHANGELOG.md)

---

## Current status

- **Public release:** `v0.1.0-beta.1`
- **Development branch:** `main`
- **Current development line:** `0.1.0-beta.1.dev.*`
- **Platforms:** Linux x86_64 and ARM64
- **License:** MIT

Orbis is still evolving. The focus is not on supporting every possible package ecosystem at any cost; it is on making the supported ones **clear, predictable, safe, and pleasant to use**.

---

<div align="center">

### ◈ ORBIS

**Your Linux software, in one place.**

Built for people who like Linux — but do not want package management to feel fragmented.

[Releases](https://github.com/iamaritrasaha/orbis/releases) · [Issues](https://github.com/iamaritrasaha/orbis/issues) · [Contributing](CONTRIBUTING.md)

</div>
