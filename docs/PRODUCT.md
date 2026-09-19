# Orbis product definition

**Orbis — Your Linux software, in one place.**

A calm, terminal-native way to find, understand, update, and safely manage
software on Linux across the systems already trusted by the machine.

## Supported systems

| Orbis view | Native system | Scope |
| --- | --- | --- |
| Ubuntu repositories | APT (Nala detected as optional frontend) | System |
| Flatpak | Flatpak | System or user |
| Snap Store | Snap / snapd | System |
| Rust tools | Cargo (`cargo install`) | User-wide |
| Node.js tools | npm, pnpm (global mode) | User-wide |
| Python tools | uv tools, pipx | User-wide |

## Product principles

1. **Terminal-native.** Inline commands, transient progress, scrollback
   preserved, instant startup. The persistent full-screen interface
   (`orbis ui`) is optional, never required.
2. **Beginner-friendly.** `find`, `show`, `install`, `remove`, `update`,
   `refresh`, `clean`, `history`, `commands`, `health` cover ordinary intent;
   advanced commands remain for power users.
3. **Native managers stay authoritative.** Orbis plans, explains, confirms,
   executes narrowly, verifies, and records. It never replaces or reimplements
   a package manager.
4. **Not a package format, not a dependency resolver.** No new archive, no
   repository, no project graph management.
5. **Review before mutation.** Exact plans, typed operations, exact
   confirmation semantics (`Y/n`; literal `YES` for high-impact).
6. **Narrow privilege.** Administrator authorization only when the selected
   operation requires it; developer ecosystems are never elevated.
7. **Durable history.** Sanitized records under `$XDG_STATE_HOME/orbis`.
8. **Deterministic output.** Plain and JSON modes are stable, ANSI-free, and
   script-safe. `NO_COLOR`, `REDUCE_MOTION`, `TERM=dumb`, and non-TTY
   environments degrade cleanly.
9. **Private and local.** No telemetry, no cloud, no network except the
   user-initiated self-update check. Shell-history insights are computed
   locally, show sanitized signatures only, and are switchable off.
10. **Honesty.** Ambiguity stops the operation; unknown facts remain unknown;
    coverage limitations are separated from execution risk.

## Command language

Everyday: `orbis` (launcher) · `find` · `show` · `install` · `remove` ·
`update` (read-only) · `update --apply` · `refresh` · `clean` · `history` ·
`commands` · `health` · `self-update` · `ui`.

Advanced/compatibility: `search`, `info`, `explain`, `updates`, `doctor`,
`upgrade`, `sources`, `why`, `dashboard`.

`update` is a read-only check by default; mutation requires `--apply` or an
explicit plan flag. This asymmetry is intentional and documented in
`docs/DECISIONS.md`.
