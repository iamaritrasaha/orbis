# Architecture decisions

Durable, factual records of the decisions that shape Orbis. Newest decisions
may reference older ones; do not delete entries — supersede them.

---

## ADR-001 — APT backend strategy (2026-09-20)

**Status:** accepted.

**Context.** APT is the most important system provider. Orbis historically
parsed `apt-cache`/`apt-get -s` output; the question was whether to move to
`rust-apt`/`libapt-pkg` directly (the path Nala's Rust rewrite is taking).

**Investigated.**

- `rust-apt` 0.11.x (volian): rich API (candidate/installed versions,
  `is_upgradable`, auto/garbage marks, hold marks, download/disk sizes,
  `get_changes`, resolver, origins) — functionally attractive.
- **License:** `GPL-3.0-or-later`. Orbis is MIT-only. Distributing Orbis
  linked with rust-apt would require the combined work to satisfy
  GPL-compatible distribution terms, which conflicts with Orbis's current
  MIT-only distribution goal. This alone blocks adoption today.
- **Build/runtime:** requires `libapt-pkg-dev` ≥ 2.0.2 (C++ build dep) and
  libapt linkage at runtime — heavier CI and release artifacts.
- **Stability:** upstream states the API "is subject to change at any time"
  (breaking changes on minor versions) and that the crate "is not advised to
  be used in multiple threads", while Orbis's registry queries providers in
  parallel threads per operation.
- **Ecosystem direction:** Debian plans Rust dependencies in APT itself from
  2026; Nala's rewrite is in transition — the library path will mature, but
  it is not stable today.

**Decision: Option D — hybrid, no new dependency.** Keep MIT and use each
machine-facing APT interface for what it is authoritative for:

1. `apt-cache show` — RFC822 package records (metadata, descriptions).
2. `dpkg-query -W -f=...` — structured installed-state and version facts.
3. `apt-get -s` — the resolver's own simulation protocol (`Inst`/`Remv`/
   `Conf` lines, "Need to get", "After this operation"); this *is* libapt-pkg
   computing the plan, not human-presentation parsing. Parsed defensively,
   fail-closed on anomalies, removals block the plan.
4. `apt-mark showhold/showauto/showmanual` — read-only mark facts.

Rules that came with the decision:

- Exactly one upgrade simulation per read/planning step
  (`AptProvider::upgrade_facts`); verification may re-simulate once after a
  mutation — that is a new state check, not planning redundancy.
- Hold status and security relevance are facts, never guesses: security is
  derived only from a reported suite (`*security*`, `*-esm`), otherwise
  unknown.
- No `full-upgrade`/`dist-upgrade`/`autoremove` side effects inside upgrade
  execution; simulation-reported removals block the plan.

**Revisit trigger.** When any of these changes, re-open this decision: rust-apt
relaxes or dual-licenses; Orbis's owner chooses to relicense; rust-apt reaches
a stable 1.x with thread-safety guidance; or a required APT fact becomes
impossible through the CLI interfaces.

---

## ADR-002 — `update` stays read-only; mutation is explicit (2026-09-20)

**Status:** accepted.

`orbis update` is a read-only check; `orbis update --apply` (or the
compatibility `orbis upgrade`) plans, revalidates, confirms, executes, and
verifies. Evaluation from a beginner's perspective: the most dangerous thing a
software manager can do is mutate on a bare verb. The asymmetry costs one
flag and buys "the default never changes your machine". Compatibility aliases
(`updates`, `upgrade`) keep existing scripts working. **Not** changed for
aesthetic preference.

---

## ADR-003 — Shell-history insights are sanitized, local, and opt-out (2026-09-20)

**Status:** accepted.

The `commands` feature derives "most used commands" from local shell history.
Design invariants:

- Read-only; never executed; never persisted; never networked.
- Signatures only: executable + (when provably safe) one benign subcommand.
  Paths, URLs, option values, environment values, credentials, and composite
  lines never appear; uncertain input degrades to the executable alone.
- Bash history resolution is bash-specific: `ORBIS_BASH_HISTFILE` (explicit
  Orbis override, existing regular file) first; then `$HISTFILE` only when it
  is an existing regular file under the user's home directory (symlinks
  resolved) with Bash evidence — a Bash-history file name or a Bash login
  shell — and its name does not itself identify zsh or fish history; then
  `~/.bash_history`; otherwise the shell is honestly unavailable. A foreign
  `HISTFILE` is never parsed as Bash. `#<epoch>` timestamp lines are
  metadata, never commands.
- Disabled via `ORBIS_HISTORY_INSIGHTS=off`; absent history degrades to an
  empty, honest result.
- Storage is separate from, and never merged with, Orbis transaction history
  (`orbis history` = what Orbis changed; `commands` = what the user tends to
  run).

---

## ADR-004 — CLI backends are acceptable when the CLI is the authoritative API (2026-09-20)

**Status:** accepted, applies to all providers.

Libraries are adopted when they make Orbis more *correct*, not merely to avoid
subprocesses. Per-provider choices: Flatpak (documented `--columns` CLI),
Snap (`snap` CLI; snapd's local HTTP API exists but adds an auth/socket
surface the CLI already wraps — revisit if per-app facts are ever needed that
only the API exposes), Cargo/npm/pnpm/uv/pipx (JSON surfaces where the tools
provide them, exact CLI for mutations). Developer providers are user-wide
only; Orbis never writes project manifests, lockfiles, `node_modules`,
or `.venv`.

---

## Provider capability matrix (audit of record, 2026-09-20)

| Provider | Read source of truth | Plan quality | Privilege | Verification | Known limitations |
| --- | --- | --- | --- | --- | --- |
| APT | apt-cache RFC822, dpkg-query, `apt-get -s`, apt-mark | Authoritative simulation (sizes, kept-back, removals block) | Administrator | Installed-state via dpkg-query facts | Metadata freshness unknown without refresh (reported honestly); phased updates deferred by APT itself |
| Flatpak | `flatpak list/search/info --columns` | Partial (runtimes resolved at commit; remote resolved per-scope) | System/User split | Scoped installed-state | Upgrade not auto-executable (no zero-action commit proof); cleanup blocked (no safe unused-ref enumeration) |
| Snap | `snap list/info/find`, `refresh --list` | Partial (store resolves at commit) | Administrator | Installed-state + pending-refresh recheck | Retention/schedules owned by snapd; refresh can race with automatic refreshes (revalidated) |
| Cargo | `cargo install --list`, `cargo info` | Partial metadata plan; no dry run upstream | None (user) | `cargo install --list` | Upgrade blocked: install provenance (git/path/registry) cannot be proven |
| npm | `npm ls -g --json`, `outdated --json`, `view --json` | Complete via documented `--dry-run` | None (writable prefix required) | Global list | Newer-than-latest excluded (downgrade hazard); sudo global installs refused |
| pnpm | `pnpm list -g --json`, `outdated --format json` | Partial (no authoritative global dry run) | None (writable bin on PATH) | Global list | Setup/PATH never rewritten by Orbis |
| uv | `uv tool list (--outdated)` | Partial plan; upgrade uses uv's own constraints | None (writable tool bin) | Tool list | Fuzzy search intentionally unsupported |
| pipx | `pipx list --output json` | Complete inventory; pinned apps respected | None (user bin) | JSON snapshot | Fuzzy search intentionally unsupported |
