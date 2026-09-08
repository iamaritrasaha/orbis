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
  self-update Update Orbis safely

EXAMPLES

  orbis find firefox
  orbis show btop
  orbis install btop
  orbis update
  orbis health
```

Run `orbis <command> --help` for details. The older names `search`, `info`, `explain`, `updates`, and `doctor` remain available as compatibility aliases where applicable. `upgrade` remains the advanced compatibility command for applying an update plan.

## Dashboard preview

The home screen uses a selectable command list, aligned shortcuts, and a compact software-management pulse. Values below illustrate the layout; the running view uses discovered sources, update results, and recorded history.

```text
                            ◈ ORBIS
                 Your Linux software, in one place.

  ──────────────────────────────────────────────────────────────────
   ▸ FIND SOFTWARE                                             [/]
     Search applications, tools and packages
     UPDATES                                                   [U]
     4 updates available
     CLEAN                                                     [C]
     Review unused software safely
     HEALTH                                                    [H]
     Everything looks good

  ──────────────────────────────────────────────────────────────────
  SYSTEM PULSE
  sources 6/8 ready  /  4 updates available
  RECENT  no Orbis changes recorded yet

  ──────────────────────────────────────────────────────────────────
  ↑↓ move  Enter open  / Find  U updates  A advanced  ? Help  Q Quit
```

Use Up/Down or j/k and Enter, or the direct shortcuts. The five-line wordmark appears only during the short dashboard startup reveal; direct operations use a compact reveal of about 320 ms. Any key skips the reveal. At 80 columns the same command list and footer remain visible. Wide layouts limit the home content width instead of stretching empty cards.

## Live operations

Orbis restores the normal terminal before requesting administrator authentication, then returns to the live operation view. Run `orbis refresh` normally; no preparatory authorization command is needed. User-level plans do not request administrator access.

```text
◈ ORBIS  /  Refreshing software information
3 sources · staged safely

SOFTWARE SOURCES
●  Ubuntu repositories Done
⠹  Flatpak apps        Working…
○  Snap Store          Waiting

STAGE
● Preparing
● Permission granted
⠹ Refreshing software information …
○ Checking software information
○ Finishing up

1 done   1 active   1 waiting   0 problems
──────────────────────────────────────────────────────────────────
D details   L details   ? Help
```

The summary receives real provider start, output, and completion events. Providers that need no refresh report `Not needed`; Snap refresh is managed automatically. Unsupported plans are marked `Skipped safely`. Stable APT `Hit:`, `Get:`, `Ign:`, and `Err:` lines provide a best-effort source URL and activity label. This parsing affects presentation only, and unknown lines remain available in Details.

Press D or L for a separate scrolling log of the most recent 200 provider output lines, then press it again to return to the summary. The summary reserves no empty log rectangle. Exact percentages are never invented. Ctrl+C during a provider operation requests exit after the operation finishes safely. Cached administrator credentials are checked noninteractively during execution; expired credentials fail safely and require a retry.

The active spinner advances at about 10 Hz. `REDUCE_MOTION` disables motion; `NO_COLOR` disables color and startup reveal. `TERM=dumb`, `--plain`, and non-TTY output use plain presentation. The idle interface stops animation redraws once background work and startup are finished.

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

## Updating Orbis itself

Orbis can check and update a user-local Orbis installation from the official GitHub Releases channel:

```sh
orbis self-update --check
orbis self-update
```

The normal flow shows the current and available versions, asks before downloading, verifies the SHA-256 checksum and archive structure, and atomically replaces only the executable that is actually running. The existing binary remains untouched if any check fails. `--yes` skips only the Orbis confirmation after the exact release has been selected; it never bypasses checksum verification, archive safety, or provider authorization.

Self-update does not use `sudo`, does not run a downloaded installer script, and cannot replace a system-owned installation or another `orbis` found on `PATH`. For those installations, use the original install method. The supported release channel is the current Beta channel, with Linux x86_64 and ARM64 archives.

Development builds contain `.dev.` in their version and never replace themselves. They report the safe source workflow instead:

```sh
git pull --ff-only
cargo install --path crates/orbis-cli --locked --force
```

The release check is bounded and quiet during normal dashboard startup. If a new public release is known, the dashboard shows a small review notice; it never updates Orbis in the background. No account, telemetry, or hosted service is required beyond the public GitHub release request.

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
cargo install --path crates/orbis-cli --locked
```

Use `--offline` when all locked dependencies are already cached. A source build installs to Cargo's user bin directory; a release installer uses `~/.local/bin`. If more than one `orbis` is installed, run `command -v orbis` and `type -a orbis`, then invoke the path you intended. Orbis self-update only considers the executable returned by `current_exe`, not whichever copy happens to appear first on `PATH`.

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
