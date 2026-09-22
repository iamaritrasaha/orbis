# AGENTS.md — working on Orbis

This file is the durable handoff for any agent or contributor taking over the
repository. Read it before changing code.

## What Orbis is

Orbis is a terminal-native Linux software manager that gives APT, Flatpak,
Snap, Cargo, npm, pnpm, uv tools, and pipx one consistent interface while the
native package managers remain authoritative. Orbis is not a package format,
not a repository, and not a dependency resolver.

## Non-negotiable product principles

- Terminal-native, beginner-friendly, calm presentation.
- No shell command construction anywhere in production execution; typed
  mutations only (`CommandSpec` argument vectors, no `sh -c`, `bash -c`, `eval`).
- Review before mutation; blocked plans cannot cross the confirmation boundary;
  `--yes` skips only Orbis's prompt, never provider safeguards.
- Narrow privilege escalation (`sudo -n` preflight, explicit authorization,
  no developer-provider sudo ever).
- Durable, sanitized history; deterministic plain/JSON output; `NO_COLOR`,
  `REDUCE_MOTION`, `TERM=dumb`, non-TTY safety.
- No cloud, no telemetry, no AI/agent branding in product code or docs.
- Ambiguity is never silently resolved; unknown facts stay unknown.
- Never fabricate provider facts (versions, sizes, security flags) — leave
  them `None` and mark coverage honestly.

## Where things live

- `crates/orbis-core` — models, provider contracts, planning, transaction and
  maintenance execution, privilege boundary, **operation journal**, APT
  outcome observation/diagnosis, system insight, legacy history store,
  optional shell-history insights, diagnostics.
- `crates/orbis-cli` — command parsing, orchestration, plain/transient
  rendering (Recent Activity), persistent TUI, self-update.
- `docs/PRODUCT.md`, `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`,
  `docs/ROADMAP.md`, `docs/CURRENT_STATE.md` — durable context.
  `CURRENT_STATE.md` is the single source of "where are we now".

## Source of truth per provider (do not regress these)

- APT: `apt-cache` (RFC822 records), `dpkg-query -W -f` (structured status),
  `apt-get -s` (simulation protocol), `apt-mark showhold/showauto/showmanual`.
  One simulation per operation — use `AptProvider::upgrade_facts`.
- Flatpak: documented `--columns` CLI output; Flatpak upgrade is deliberately
  not auto-executable (no zero-action commit proof).
- Snap: `snap find/list/info/refresh --list`; snapd owns retention and
  schedules.
- Cargo/npm/pnpm/uv/pipx: JSON or documented CLI surfaces; user-wide only;
  never touch project manifests, lockfiles, `node_modules`, or `.venv`.

## Validation gate (run before any commit)

~~~bash
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
git diff --check
~~~

Install the current development build with
`cargo install --path crates/orbis-cli --locked --force` (no sudo) and verify
`$HOME/.cargo/bin/orbis --version`.

Read-only host QA (`orbis health`, `orbis update`, `orbis commands`, ...) is
encouraged. Never run real package mutations, refreshes, or cleanup without
explicit human approval, and never invoke sudo autonomously.

## Versioning and releases

- Development line `0.1.0-beta.1.dev.N`: bump N only for coherent milestones,
  not per commit. Do not tag, publish, or touch Beta 1 artifacts. Beta 2
  requires an explicit human decision.
- Development builds (`.dev.`) never self-replace through `orbis self-update`.

## Known open items

Always check `docs/CURRENT_STATE.md` (known failures, manual QA remaining,
exact next step) before planning work.
