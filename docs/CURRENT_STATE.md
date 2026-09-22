# Orbis current state

Updated: 2026-09-22. Baseline: `0.1.0-beta.1.dev.18` final outcome-integrity
correction on `flagship-dev18`. Not merged; no `dev.19`.

## Version

`0.1.0-beta.1.dev.18` (development line; no tag, no release, Beta 1 untouched).

## Completed work (this session)

Final outcome-integrity correction (four semantic defects fixed on top of the
earlier dev.18 correction; no regressions to it):

- dpkg non-installed states are authoritative absence, not probe failures.
  `dpkg-query` rows such as `deinstall ok config-files` or
  `purge ok not-installed` now parse as valid rows, so a removal of a
  known-but-removed package verifies as `Absent` instead of failing the probe.
  The parser distinguishes valid installed rows, valid non-installed rows,
  malformed lines (fail closed → `ParseFailed`), absent-stderr, and query
  failure. Malformed output is never treated as absence.
- `dpkg-query` observation runs under `LC_ALL=C` so dpkg status/error parsing
  is deterministic regardless of host locale; a test asserts the generated
  `CommandSpec` pins the locale.
- Dependency health is honestly tri-state. Only observed evidence
  (`BrokenDependencies`, `InterruptedDpkg`) produces `Broken`; locks,
  permission failures, unavailable commands, and unclassified errors produce
  `Unknown` with the summary retained. Uncertainty is never converted into
  broken or healthy.
- Maintenance verification is aggregated explicitly. The journal report is
  computed over executed providers only (any failed → `Failed`, else any
  partial → `PartiallyVerified`, else all verified → `Verified`, else
  incomplete); skipped/blocked providers contribute neither evidence nor
  doubt. `verified` is always consistent with `result`, so no record can
  serialize `result = verified` with `verified = false`.
- Coverage is distinguished from verification in summaries: executed
  verification incomplete → "System update completed; package-level
  verification incomplete"; executed providers verified with others
  skipped/blocked → "N packages upgraded and verified; some providers were
  not covered"; an executed provider failure → "System update partially
  failed".

Outcome-integrity correction from earlier in this line (preserved):

- APT installed-version observation is typed (`Observed` vs `Unavailable`).
  dpkg-query failure, missing binary, and parse failure no longer collapse
  into an empty map that verified removals.
- APT update summaries: "already up to date" only when the reviewed plan had
  zero candidates. Incomplete package-level observation is
  `PartiallyVerified`, never inferred from an empty change list.
- Mutating maintenance writes `OperationStatus::Running` fail-closed before
  the executor; CLI and TUI share that lifecycle. Journal start/end reuse one
  timestamp. Final journal write failures are surfaced, not ignored.
- Journal verification checks carry actual observation evidence. Non-APT
  records never claim "via dpkg".
- Health is tri-state: probe/inventory failure renders `unknown`, not ok/zero.

Product-model from the same development line: Orbis thinks in **operations /
outcomes**, not shell commands.

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
- `cargo test --workspace`: 243 tests green (92 CLI + 151 core; was 233).
- `cargo build --workspace --release` succeeds.
- `git diff --check` clean.
- Read-only host QA: `orbis health` renders tri-state dependency health; the
  host's real `deinstall ok config-files` dpkg rows match the new parser.

## Known failures

Release workflow `plan` job fails on PRs: `dist plan` refuses
`.github/workflows/release.yml` as stale because a curated GitHub Release
notes block was added after cargo-dist generated the file. cargo-dist 0.32.0
wants the stock `gh release create` notes path. Not changed in this
correction (release infrastructure left untouched). Standing product
limitations remain by design: Flatpak upgrades are not auto-executable; Cargo
upgrade remains blocked; non-APT providers still use lighter verification
than the APT vertical slice.

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
