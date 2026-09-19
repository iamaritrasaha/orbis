# Orbis current state

Updated: 2026-09-20. Baseline commit before this session: `c54ac9d`.

## Version

`0.1.0-beta.1.dev.16` (development line; no tag, no release, Beta 1 untouched).

## Completed work (this session)

- **APT engine hardening.** One `apt-get -s upgrade` simulation per
  read/planning step (previously three in a single plan path), hold marks via
  read-only `apt-mark showhold`, and candidate suite provenance with
  security-relevance derived only from reported suites.
- **History resilience.** `orbis history` skips unreadable or corrupt record
  files instead of failing the whole listing.
- **Shell-history insights (`orbis commands`).** New local-only, privacy-
  sanitized subsystem (`orbis-core::shell_history`) with Bash source, strict
  signature sanitizer, `--limit/--shell/--json`, `ORBIS_HISTORY_INSIGHTS`
  opt-out, launcher/home "Frequent commands" section (max 3), completions and
  man page updates.
- **Health expansion.** Sudo availability, history directory, shell-history
  insight state, and self-update eligibility reported alongside provider
  diagnostics; optional-provider gaps stay non-catastrophic.
- **Reliability fixes.** `Ign` APT source lines no longer render as "up to
  date" during execution (they read as "skipped"); dead code removed
  (unreachable transaction wrapper, stale allows).
- **Durable docs.** `AGENTS.md`, `docs/PRODUCT.md`, `docs/DECISIONS.md`
  (APT decision + provider capability matrix + update-semantics decision),
  canonical `docs/ARCHITECTURE.md` / `docs/ROADMAP.md`, this file.

## Validation performed

- `cargo fmt --all -- --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace`: 189 tests green (was 171 at baseline).
- `cargo build --workspace --release` succeeds.
- Read-only host QA: `orbis commands` (human + JSON + disabled + unsupported
  shell), `orbis health`, `orbis update` + `--json` inventory inspection,
  launcher rendering, apt holds/suite parsing against the live system.

## Known failures

None known at this commit. Two long-honest limitations are by design and
documented: Flatpak upgrades are not auto-executable (no zero-action commit
proof), and Cargo upgrade remains blocked (install provenance is unprovable).

## Manual QA remaining (needs a human; Orbis performed none of these)

~~~bash
orbis update --apply        # confirm review, revalidation, sudo preflight
orbis refresh               # metadata refresh across providers
orbis clean --plan          # autoremove candidates listing
orbis install <pkg>         # single-package transaction end to end
orbis remove <pkg>          # removal + verification
orbis                       # launcher with Frequent commands section
~~~

## Exact next step

Beta 2 preparation requires a human decision (see ROADMAP.md Milestone 6).
Candidate engineering work before that: Snap's structured local API
evaluation (ADR-004 revisit note), per-provider latency instrumentation for
the `update` path, and a small optional config file only if user feedback
demands one.
