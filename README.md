# Orbis

Your Linux software, in one place.

Orbis is a calm, beginner-friendly way to find, understand, update, and safely manage software on Linux. You use the software name you know; Orbis works with the local software systems already trusted by your distribution and keeps their technical details available when you need them.

The public release is [0.1.0-beta.1](https://github.com/iamaritrasaha/orbis/releases/tag/v0.1.0-beta.1). The current `main` branch is the next development snapshot and reports a development suffix until a later release is made. Orbis is Linux-first for x86_64 and ARM64. It does not require a hosted service or telemetry.

## Start here

```sh
orbis find firefox
orbis show btop
orbis install btop
orbis update
orbis refresh
orbis clean
orbis history
orbis health
```

`orbis` on its own opens the interactive dashboard in a capable terminal. Every change is reviewed before it runs unless you explicitly use `--yes` after resolving an exact plan.

## Help for beginners

```text
COMMON COMMANDS

  find       Find software
  show       Learn about software
  install    Install software
  remove     Remove software
  update     Check for updates
  refresh    Refresh software information
  clean      Remove unused software safely
  history    See previous Orbis actions
  health     Check that everything is working

EXAMPLES

  orbis find firefox
  orbis show btop
  orbis install btop
  orbis update
  orbis health
```

Run `orbis <command> --help` for details. The older names `search`, `info`, `explain`, `updates`, and `doctor` remain available as compatibility aliases where applicable. `upgrade` remains the advanced compatibility command for applying an update plan.

## Dashboard preview

The dashboard is task-led rather than a list of package-manager diagnostics. At the primary 121×24 terminal size it keeps the main choices, recent activity, and navigation together:

```text
                                  ◈ ORBIS
                         Your Linux software, in one place.

  What would you like to do?

  ╭────────────────────────────────────╮  ╭────────────────────────────────────╮
  │ [F] Find software                   │  │ [U] Updates                         │
  │     Search apps and tools           │  │     4 updates available             │
  ╰────────────────────────────────────╯  ╰────────────────────────────────────╯
  ╭────────────────────────────────────╮  ╭────────────────────────────────────╮
  │ [C] Clean up                       │  │ [H] Health                          │
  │     Review unused software safely   │  │     Everything looks good           │
  ╰────────────────────────────────────╯  ╰────────────────────────────────────╯

  RECENT ACTIVITY
  No recent Orbis activity
  Updates are checked without changing software

────────────────────────────────────────────────────────────────────────────────
  / Find   U Updates   C Clean   H Health   A Advanced   ? Help   Q Quit
```

The five-line wordmark is reserved for the short startup reveal. Once the dashboard is ready, Orbis uses the compact `◈ ORBIS` identity so the interface has room for useful work. Narrow terminals stack the same tasks and preserve the footer.

## Live operations

Refresh, update application, cleanup, install, and removal use a short, persistent operation view in an interactive TTY:

```text
◈ ORBIS  /  Refreshing software information
2 sources · staged safely

PROGRESS                    SOURCES
● Preparing                 ● Ubuntu repositories  Done
◐ Getting permission       ◐ Flatpak apps          Working…
○ Installing               ○ Snap Store            Waiting
○ Checking installation
○ Finishing up

┌──────────────────────────────────────────────────────────────────────────────┐
│ PROVIDER OUTPUT · DETAILS                                                     │
│ Get:1 ...                                                                     │
└──────────────────────────────────────────────────────────────────────────────┘
```

The view reports real lifecycle and provider events, shows update results as each source finishes, keeps raw provider output in a bounded details area, and never invents a percentage. It uses a small spinner only while work is active; `REDUCE_MOTION`, `NO_COLOR`, `TERM=dumb`, `--plain`, non-TTY output, and keypresses provide quiet deterministic fallbacks.

## Finding and understanding software

```sh
orbis find btop
orbis show btop
```

Search results lead with the package name and purpose. The Orbis Brief explains what software does, why it may be useful, whether it is installed, and where it came from. Explanations are evidence-aware: when Orbis cannot establish a fact, it says so instead of guessing.

If the same name is available from several places, Orbis never silently chooses. Interactive terminals show a source chooser with a reasoned recommendation; scripts and other non-interactive callers must use `--source` or a qualified reference such as `apt:btop`.

## Updates and software information

`orbis update` is a read-only check. It reports available updates and offers a clear route to review and apply them. It does not refresh catalogs and does not change installed software.

```sh
orbis update                 # check only
orbis update --plan          # build a read-only application plan
orbis update --apply         # review, confirm, execute, and verify
orbis refresh                # refresh software information
orbis refresh --plan         # inspect the refresh plan only
orbis upgrade --plan         # advanced compatibility spelling
```

`orbis update --yes` applies the exact reviewed update operation without the second confirmation prompt. `--yes` never bypasses provider safeguards or administrator authorization.

## Safety

Orbis separates discovery, planning, confirmation, execution, verification, and history:

- ambiguous software names are stopped rather than guessed;
- plans are read-only and show the intended changes, scope, privilege, warnings, and confidence;
- incomplete or blocked plans cannot cross into execution;
- system changes use a narrow typed administrator boundary and never expose passwords;
- process arguments remain structured and never pass through a shell;
- cleanup is conservative and does not purge configuration, package caches, application data, or Snap revisions;
- every attempted change is recorded without command output, credentials, or tokens.

JSON output is a stable machine-readable interface. It remains provider-precise and is not rewritten to imitate the beginner presentation.

## Software systems Orbis can use

The normal interface uses friendly names. Advanced views and `--source` expose exact provenance when it matters.

| Beginner-facing name | What Orbis can do |
| --- | --- |
| Ubuntu/Debian repositories | Find, show, install, remove, refresh, update, and conservative cleanup |
| Flatpak apps | Find, show, install, remove, refresh, and update where safe |
| Snap Store | Find, show, install, remove, and report pending updates |
| Rust tools | Find, show, install, remove, and inspect user-wide Cargo tools |
| Node.js tools | Find, show, install, remove, and inspect global npm/pnpm tools |
| Python tools | Show, install, remove, and inspect persistent uv/pipx tools |

Provider-specific source names, capabilities, limitations, and raw metadata are available under `orbis sources`, `orbis health`, and the Advanced area of the dashboard. Developer-tool operations are user-local; Orbis does not edit project manifests, lockfiles, `node_modules`, virtual environments, or arbitrary Python environments.

## Installation

### Release installer

The Beta 1 installer installs to the user-owned `~/.local/bin` directory and does not need `sudo`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/iamaritrasaha/orbis/releases/download/v0.1.0-beta.1/orbis-installer.sh | sh
```

Restart the shell or follow the installer’s PATH instruction if `orbis` is not immediately found. Installing Orbis is separate from a later system software operation requesting administrator authorization.

### Manual archive

Download the matching Linux archive and checksum from the [Beta 1 release](https://github.com/iamaritrasaha/orbis/releases/tag/v0.1.0-beta.1), verify it, then install locally:

```sh
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

Requires stable Rust and the locked workspace dependencies:

```sh
cargo install --offline --path crates/orbis-cli --locked
```

Or run a release build without installing:

```sh
cargo build --workspace --release
./target/release/orbis
```

## Advanced usage

```sh
orbis sources
orbis health
orbis search ripgrep
orbis info apt:libssl-dev
orbis explain apt:libssl3
orbis why apt:libssl3
orbis upgrade --plan
orbis clean --plan
orbis install cargo:ripgrep --plan
orbis --json search btop
```

Compatibility commands remain useful for existing scripts. Use `orbis --help` and each command’s help for the exact current options.

## Development

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
git diff --check
```

Tests use provider fixtures, TestBackend rendering, mocked command runners, and fake operation executors. They do not install or remove software, refresh catalogs, require `sudo`, or need a graphical desktop.

The workspace is organized as:

```text
crates/orbis-core/    models, providers, planning, execution, history, safety
crates/orbis-cli/     parser, commands, plain output, live TUI, shared theme
docs/                 architecture, maintenance, transactions, providers, releases
```

Read [CONTRIBUTING.md](CONTRIBUTING.md), [docs/architecture.md](docs/architecture.md), and [docs/maintenance.md](docs/maintenance.md) before changing provider or transaction behavior. Historical release notes remain in [docs/releases.md](docs/releases.md).

Orbis is released under the MIT license.
