# Orbis maintenance

Orbis presents maintenance as user goals first. The commands below keep discovery, planning, confirmation, execution, verification, and history separate.

## Beginner command vocabulary

- `orbis update` checks for available software updates. It is read-only and does not refresh catalogs or change installed software.
- `orbis update --plan` builds a read-only plan for applying the updates.
- `orbis update --apply` reviews, confirms, executes, verifies, and records the update operation.
- `orbis refresh` refreshes software information and catalogs. It does not upgrade installed software.
- `orbis refresh --plan` shows the refresh work without changing catalog metadata.
- `orbis clean` reviews confidently unused package-manager-owned items. Cleanup is never a general disk cleaner.
- `orbis history` reads sanitized Orbis operation records.
- `orbis health` runs read-only diagnostics.

The advanced compatibility command `orbis upgrade` remains available for applying a reviewed update plan. The old `updates` spelling remains an alias for the read-only `update` check; `doctor` remains an alias for `health`. The older `search`, `info`, `explain`, and `why` commands remain available for scripts and advanced inspection.

All maintenance commands accept `--source apt`, `--source flatpak`, `--source snap`, `--source cargo`, `--source npm`, `--source pnpm`, `--source uv`, or `--source pipx`. The global `--json` option exposes provider-precise structured data without ANSI styling or live terminal control sequences.

## Updating Orbis

`orbis self-update --check` performs a read-only check against the canonical Orbis GitHub Releases channel. `orbis self-update` reviews and, after confirmation, downloads the matching Linux x86_64 or ARM64 archive, requires its SHA-256 checksum, rejects unsafe archive entries, and atomically replaces only the user-owned executable returned by `std::env::current_exe()`. It never uses `sudo`, executes a downloaded script, or replaces another installation.

Development builds contain `.dev.` and are deliberately ineligible for self-replacement. They report `git pull --ff-only` followed by `cargo install --path crates/orbis-cli --locked --force`. An explicit `orbis ui` launch may perform a bounded background release check at most once per 24 hours and only displays a subtle notice; it never mutates Orbis.

## Live operations

Human-facing commands use a transient Orbis execution view that returns to the shell after completion. It reports the universal stages `Preparing`, `Getting permission`, `Installing`, `Checking installation`, and `Finishing up`, along with source activity and bounded provider detail. The persistent execution view remains available through explicit `orbis ui` navigation.

Provider activity comes from typed core events around the provider operation. A source can be waiting, working, done, or in need of attention. Update inventories are delivered to the TUI as each source finishes, while the final report is sorted for deterministic plain and JSON output. Orbis does not infer safety, counts, verification, or percentages by scraping display text. Raw stdout and stderr are retained only in memory for the details panel and are bounded before rendering.

The spinner advances at roughly 10 Hz only while a capable TTY operation is active. `REDUCE_MOTION`, `NO_COLOR`, `TERM=dumb`, `--plain`, non-TTY output, and a keypress provide quiet or deterministic alternatives. There is no continuous animation after the operation finishes.

## Provider semantics

APT update discovery uses `apt-get -s upgrade` against the current local package information. Orbis does not silently run an APT catalog refresh as part of `orbis update`. `orbis refresh` uses the provider’s refresh operation; `orbis upgrade` uses ordinary `apt-get upgrade`, never `full-upgrade` or `dist-upgrade`, and blocks a plan that reports removals. Execution uses `--no-remove`, does not purge, and does not autoremove.

Flatpak update discovery uses scoped `flatpak remote-ls --updates` with structured columns. System and per-user installations are different candidates. AppStream refresh is scoped to the selected installation. Flatpak’s optional unused-runtime cleanup is not represented as a safe automatic cleanup plan and is therefore refused rather than guessed.

Snap update discovery uses `snap refresh --list`. snapd normally checks for refreshes automatically, so `orbis update` does not call mutating `snap refresh`. An advanced update plan targets only the exact names observed in the pending list, preserves channels and holds, and is revalidated immediately before execution because automatic refreshes can race with Orbis.

Cargo means `cargo install` binary crates, not project dependencies. npm and pnpm mean global packages only. uv means persistent `uv tool` environments, and pipx means current-user applications. Developer operations remain user-local and never edit project manifests, lockfiles, `node_modules`, virtual environments, or arbitrary Python environments. Their plan is incomplete when authoritative provenance or safe dry-run evidence is unavailable.

## Coordinated, non-atomic maintenance

APT, Flatpak, Snap, and developer tools cannot form one atomic transaction. A coordinated plan contains independent provider plans and the result reports each one separately. If one provider fails after confirmation, the result may be partial while completed providers remain recorded. Authorization failure stops later administrator-scoped work; unsupported or stale plans are skipped rather than silently changed.

Update plans are revalidated before execution. If installed state, pending version or revision, scope, or hold state has changed materially, Orbis aborts and asks for a new plan. `--yes` skips only the Orbis confirmation after the exact plan has been resolved and revalidated; it never bypasses provider safeguards or administrator authorization.

## Cleanup policy

`orbis clean` does not purge configuration files, wipe the APT cache, delete Flatpak application data, delete Snap revisions or snapshots, modify `refresh.retain`, or prune developer-tool caches. APT candidates come from `apt-get -s autoremove`; high-impact names receive elevated confirmation context. Unsupported cleanup is described as unavailable, not as zero safe items.

## History and explanations

Single-package records remain readable. Maintenance runs use the same XDG transaction directory and a parent record containing provider results. Raw command output, credentials, and tokens are not persisted.

`orbis show <name>` is the beginner package-information experience. When a package is installed, it may include provider-backed installation reasoning. `orbis why <name>` remains the advanced direct explanation route. A missing Orbis history record only means Orbis did not record the installation; it does not prove how the software arrived on the system.

APT reasoning uses install marks, installed reverse dependencies, and the APT autoremove plan. Flatpak distinguishes applications from shared runtimes and extensions. Snap reports local snap facts such as type, publisher, tracking channel, and base relationship without pretending there is an APT-style dependency graph.
