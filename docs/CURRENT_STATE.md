# Orbis current state

Updated: 2026-09-20. Baseline commit before this session: `b12d59c`
(dev.16 merge).

## Version

`0.1.0-beta.1.dev.17` (development line; no tag, no release, Beta 1 untouched).

## Completed work (this session)

Post-merge correctness and privacy follow-ups to dev.16:

- **Shell-history hang fixed.** `sanitize::executable_only()` looped
  forever on composite lines with two or more leading assignments
  (`FOO=1 BAR=2 cargo test | tee out`); one such history line hung
  `orbis commands` and the bare launcher. The assignment scan now
  advances, with deadline-guarded regression tests.
- **Bash history resolution is bash-specific.** `$HISTFILE` is trusted
  only when it is an existing regular file under the user's home (after
  resolving symlinks) with Bash evidence — a Bash-history file name or a
  Bash login shell; zsh/fish-named files are rejected outright.
  `ORBIS_BASH_HISTFILE` is the explicit override (may live outside
  HOME); `~/.bash_history` remains the fallback; otherwise the shell is
  honestly unavailable.
- **No local paths in structured output.** `ShellHistoryReport` no
  longer carries the history path; `orbis --json commands` exposes
  shell, scan/analysis counts, and sanitized insights only.
  `orbis health` reports "Bash history available · local-only insights
  enabled" without naming the path.
- **APT hold facts are tri-state.** A successful `apt-mark showhold`
  yields the actual set (possibly empty → `held: Some(false)`); an
  unavailable or failing apt-mark yields `held: None` instead of a
  false "not held" fact. Hold-data failure still never blocks update
  discovery.
- **Launcher history read bounded.** Bare `orbis` reads at most a
  256 KiB recent tail of the history file; the section is labeled
  "Recent commands" because counts describe the sampled window, not
  all-time frequency. `orbis commands` keeps the complete scan.
- **History gap honesty.** `orbis history` appends "N unreadable
  history record(s) skipped" when damaged records are skipped, without
  exposing their contents; the JSON listing shape is unchanged.
- **Docs.** ADR-003 states the exact Bash resolution contract;
  ADR-001's rust-apt licensing wording is precise (GPL-compatible
  distribution terms vs MIT-only goal); ARCHITECTURE reflects the
  resolver, privacy, and launcher bounding.

## Validation performed

- `cargo fmt --all -- --check` clean.
- `cargo check --workspace` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace`: 207 tests green (was 189 at dev.16).
- `cargo build --workspace --release` succeeds.
- Read-only host QA: `orbis --version`, `orbis commands` (human, JSON,
  disabled), `orbis --json commands` inspected to contain no history
  file or home path, bare launcher, `ORBIS_HISTORY_INSIGHTS=off orbis`,
  `orbis health`, `orbis update --source apt` and `--plan` variant, and
  the dev.16 hang reproduction (control file passes; poisoned file no
  longer hangs).

## Known failures

None known at this commit. Standing limitations remain by design and
documented: Flatpak upgrades are not auto-executable (no zero-action
commit proof), and Cargo upgrade remains blocked (install provenance is
unprovable).

## Manual QA remaining (needs a human; Orbis performed none of these)

~~~bash
orbis update --apply        # confirm review, revalidation, sudo preflight
orbis refresh               # metadata refresh across providers
orbis clean --plan          # autoremove candidates listing
orbis install <pkg>         # single-package transaction end to end
orbis remove <pkg>          # removal + verification
orbis                       # launcher Recent commands section
~~~

## Exact next step

Beta 2 preparation requires a human decision (see ROADMAP.md Milestone 6).
Candidate engineering work before that: Snap's structured local API
evaluation (ADR-004 revisit note), per-provider latency instrumentation for
the `update` path, and a small optional config file only if user feedback
demands one.
