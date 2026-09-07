# Orbis maintenance

Milestones 3 and 4 give Orbis one understandable maintenance view across system, desktop, and user-wide developer-tool providers.

## Command vocabulary

- orbis update refreshes package/catalog metadata. It does not upgrade installed software. APT refreshes repository indexes; Flatpak refreshes AppStream metadata per installation scope; Snap checks pending refresh information because snapd manages store awareness automatically.
- orbis updates is read-only. It reports installed software with updates available from the current local provider metadata. It never refreshes an APT index or changes package state.
- orbis upgrade plans and applies available updates. orbis upgrade --plan is read-only. orbis upgrade --yes skips only Orbis confirmation after planning and revalidation.
- orbis clean plans conservative package-manager-owned cleanup. APT uses autoremove simulation. Flatpak exact unused-ref planning is not available through the supported safe path, so it is refused. Snap retention is left to snapd.
- orbis history reads sanitized Orbis transaction and maintenance records. It never executes a package-manager command.
- orbis why <package> explains provider evidence, dependency consumers where they can be established, and Orbis installation provenance when a record exists.

Every command has provider filtering with --source apt, --source flatpak, --source snap, --source cargo, --source npm, --source pnpm, --source uv, or --source pipx. Maintenance commands support JSON output through the global --json flag.

## Developer-provider semantics

Cargo means `cargo install` binary crates, not Cargo.toml dependencies. npm and pnpm mean global packages only. uv means persistent `uv tool` environments, and pipx means current-user pipx applications. None of these providers uses sudo; system-owned destinations are blocked and configuration is not rewritten. `orbis clean` does not prune their caches.

Cargo uses `cargo install --list`, `cargo search`, and `cargo info`; stable Cargo's dry-run remains unstable and original install provenance is not reliably recoverable through the supported interface, so automatic Cargo upgrades remain incomplete. npm uses global JSON list/search/view/outdated commands and its supported global dry run. npm candidates newer than the latest dist-tag are excluded to prevent a downgrade. pnpm uses global JSON list/search/outdated and exact global commands, with no invented dry run. uv uses `uv tool list --outdated` and exact `uv tool upgrade`, preserving uv's original constraints/settings. pipx uses its JSON snapshot and `--skip-maintenance`; pinned applications are held and existing backends are preserved.

## Provider semantics

APT update discovery uses apt-get -s upgrade against the current local package index. Orbis does not silently run apt-get update for orbis updates. Orbis upgrade uses ordinary apt-get upgrade semantics, never full-upgrade or dist-upgrade, and blocks a plan if simulation reports removals. Kept-back packages remain kept back and are shown as notes. Execution uses --no-remove, does not purge, and does not autoremove.

Flatpak update discovery uses scoped flatpak remote-ls --updates with structured columns. System and per-user installations are different candidates. Flatpak's documented flatpak update command can also offer removal of unused end-of-life runtimes. Because the current CLI path does not provide a complete non-mutating impact plan for that side effect, unified automatic Flatpak upgrades are displayed as partial and are not executed by Orbis. This is intentional.

Snap update discovery uses snap refresh --list. Snapd normally checks for refreshes automatically, so Orbis does not call mutating snap refresh to implement orbis update. Snap upgrade plans target the exact names observed in the pending-refresh list, preserve channels and holds, and are revalidated immediately before execution because automatic refreshes can race with Orbis.

APT and system Flatpak changes cross the existing typed administrator boundary. User Flatpak AppStream refreshes remain user-scoped. Real mutations have no generic wall-clock kill timeout; only bounded metadata queries and the separate authorization check have deadlines.

## Coordinated, non-atomic maintenance

APT, Flatpak, Snap, and the developer providers cannot be one atomic transaction. A unified plan contains independent provider plans, and the result reports each provider separately. If one independent provider fails after confirmation, the result can be partial while preserving the other provider results. Authorization failure stops later administrator-scoped work; an unsupported or stale provider plan is skipped rather than silently changed. Developer-provider mutations remain user-local and never trigger the administrator authorization path.

Upgrade plans are revalidated before confirmation/execution. If the installed state, pending version/revision, scope, or hold state differs materially, Orbis aborts and asks for a new plan.

## Cleanup policy

Orbis clean is not a disk cleaner. It does not purge configuration files, wipe the APT cache, delete Flatpak data, delete Snap revisions, delete Snap snapshots, or modify refresh.retain. APT candidates come from apt-get -s autoremove; high-impact names receive elevated confirmation context.

## History and why

Single-package records from Milestone 2 remain readable. Maintenance runs use the same XDG transaction directory and schema version, with a lightweight parent run containing provider results. Raw command output, credentials, and tokens are not persisted.

APT why uses install marks, installed reverse dependencies, and the APT autoremove plan. Flatpak distinguishes applications from shared runtimes/extensions and reports applications whose declared runtime matches the queried ref. Snap reports local snap info facts such as type, publisher, tracking channel, and base relationship, without pretending there is an APT-style dependency graph. Absence from Orbis history means only that no Orbis record was found; it does not prove how the package was installed.
