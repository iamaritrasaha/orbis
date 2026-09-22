# Orbis current state

Updated: 2026-09-22. Baseline before this session: `0.1.0-beta.1.dev.17`.

## Version

`0.1.0-beta.1.dev.18` (development line; no tag, no release, Beta 1 untouched).

## Completed work (this session)

Product-model correction: Orbis now thinks in **operations / outcomes**, not
shell commands.

- **Operation journal.** New durable store under
  `$XDG_STATE_HOME/orbis/operations` records one meaningful operation per user
  intent (install, remove, system update, refresh, cleanup) with status,
  changes (old→new versions), verification, warnings (including reboot),
  structured failure diagnosis, duration, and expandable raw command refs.
  Failed verification never reports as completed.
- **APT vertical slice.** Install/remove/upgrade observe dpkg state after
  mutation, record installed versions, classify common APT failures (lock,
  broken deps, repository, network, held, permission, invalid package,
  interrupted dpkg), check reboot-required and dependency health. Exit status
  alone is not treated as success.
- **Recent Activity replaces Recent Commands** on the launcher and home view.
  Shell-history signatures (`apt`, `cd`, `ls`) are no longer the primary
  surface. `orbis history` / `orbis activity` prefer the operation journal.
  Shell-history insights remain available only via `orbis commands`.
- **Actionable system insight.** `orbis health` surfaces pending APT updates,
  reboot requirement, broken dependencies, failed systemd units, disk pressure
  on important mounts, held packages, and kernel release — not a generic
  metric dashboard. Launcher shows instant attention (reboot + failed ops)
  without starting provider inventories.
- **Rust kept; Python not introduced.** Typed `CommandSpec`, privilege
  boundary, and process runner already earn Rust. Application outcome logic
  was added in-process rather than splitting runtimes.

## Validation performed

- `cargo fmt --all -- --check` clean (after fmt).
- `cargo check --workspace` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace`: 220 tests green (91 CLI + 129 core; was 207).
- `cargo build --workspace --release` succeeds.
- `git diff --check` clean.

## Known failures

None known at this commit. Standing limitations remain by design: Flatpak
upgrades are not auto-executable; Cargo upgrade remains blocked; non-APT
providers still use lighter verification than the APT vertical slice.

## Manual QA remaining (needs a human; Orbis performed none of these)

~~~bash
orbis update --apply        # confirm one System updated journal entry + versions
orbis install <pkg>         # confirm version-verified install outcome
orbis remove <pkg>          # confirm removal verification
orbis                       # launcher Recent activity / Needs attention
orbis activity              # operation journal listing
orbis health                # actionable system insight
~~~

## Exact next step

Deepen APT-only product usefulness before expanding providers: richer
post-upgrade change lists from dpkg logs when simulation candidates are empty,
service restart observation, and optionally demote or hide shallow multi-provider
surfaces that do not yet produce verified outcomes. Beta 2 remains a human
decision.
