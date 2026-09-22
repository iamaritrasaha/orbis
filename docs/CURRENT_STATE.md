# Orbis current state

Updated: 2026-09-22. Baseline: `0.1.0-beta.1.dev.18` terminal UX and
outcome-integrity correction on `flagship-dev18`. Not merged; no `dev.19`.

## Version

`0.1.0-beta.1.dev.18` (development line; no tag, no release, Beta 1 untouched).

## Completed work (this session)

Terminal UX and zero-candidate update correction:

- A complete, supported upgrade plan with zero candidates is now a successful
  CLI no-op. It returns exit 0, emits a concise human result or structured JSON,
  and stops before confirmation, administrator authorization, executor,
  history, or operation-journal mutation. Blocked, unsupported, incomplete, or
  candidate-bearing plans remain non-executable and fail closed.
- Default plain output now uses a compact identity line (`◈ Orbis · Title`),
  outcome-first wording, semantic marks, and whitespace hierarchy instead of
  report cards, divider bars, repeated branding, and internal lifecycle rows.
  Show, install/remove review and result, updates, activity, and health were
  redesigned while diagnostic output retains IDs, raw typed commands,
  verification, coverage, warnings, and evidence.
- Transient progress owns one or two task-oriented lines and collapses into the
  final result. The bare launcher is now a static compact summary; a real PTY
  check showed no leading blank reservation and no blank footprint on exit.
- Development self-update output distinguishes the current development build
  from the published release and explains that self-update is disabled.
- Descriptions and long package names wrap by terminal display width and break
  long words safely at narrow widths. NO_COLOR, TERM=dumb, and non-TTY paths
  remain deterministic and readable.

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
- `cargo test --workspace`: 245 tests green (91 CLI + 154 core; was 243).
- `cargo build --workspace --release` succeeds.
- `git diff --check` clean.
- Installed local binary with `cargo install --path crates/orbis-cli --locked
  --force`; `$HOME/.cargo/bin/orbis --version` reports
  `0.1.0-beta.1.dev.18`.
- Read-only host QA completed for `orbis --version`, bare `orbis`, `show apt:sl`,
  update inventory, activity, health, and self-update check. The current host's
  APT index reports unknown freshness, so `orbis update --apply --source apt`
  correctly stopped before confirmation/authorization/execution with no
  journal mutation; a physically complete zero-candidate plan was not present
  to exercise the success branch on this host.

## Known failures

Release workflow `plan` job fails on PRs: `dist plan` refuses
`.github/workflows/release.yml` as stale because a curated GitHub Release
notes block was added after cargo-dist generated the file. cargo-dist 0.32.0
wants the stock `gh release create` notes path. Not changed in this
correction (release infrastructure left untouched). Standing product
limitations remain by design: Flatpak upgrades are not auto-executable; Cargo
upgrade remains blocked; non-APT providers still use lighter verification
than the APT vertical slice.

## Manual QA remaining (needs a human)

~~~bash
orbis                       # visually review compact PTY launcher and cleanup
orbis show apt:sl           # compact show layout and natural wrapping
orbis activity              # compact verified operation listing
orbis health                # exceptions-first layout and unknown handling
orbis self-update --check   # development-build wording
orbis update --apply --source apt  # on a complete zero-candidate APT plan,
                                   # confirm exit 0, no prompt, no sudo, and
                                   # no activity/journal record
~~~

No real package mutation or sudo was performed in this session. The earlier
human-verified APT install/remove vertical slice remains the reference for
mutation QA.

## Exact next step

Have a human review the compact terminal surfaces in a real shell, then refine
any remaining persistent TUI/detail-surface inconsistencies. After that,
deepen APT-only product usefulness before expanding providers: richer
post-upgrade change lists from dpkg logs when simulation candidates are empty,
service restart observation, and optionally demote or hide shallow multi-provider
surfaces that do not yet produce verified outcomes. Beta 2 remains a human
decision.
