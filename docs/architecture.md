# Orbis architecture

Orbis is split into a small library crate and a binary crate:

~~~text
orbis-cli
  cli.rs              command parsing and typed CLI arguments
  commands.rs         command orchestration and safety-boundary calls
  render/             plain output and centralized theme tokens
  tui/                event loop, screens, workers, and Ratatui rendering
  JSON/stdout policy
        |
orbis-core
  discovery and resolution
  normalized models
  deterministic explanations
  diagnostics
  provider contract
  shell-free process runner
  typed transaction plans
  normalized maintenance plans
  narrow privilege boundary
  XDG transaction and maintenance history
        |
  APT/Nala | Flatpak | Snap | Cargo | npm | pnpm | uv tool | pipx
~~~

## Provider boundary

The separate MaintenanceProvider contract normalizes update inventories, provider-scoped upgrade plans, conservative cleanup candidates, and explanation evidence. It keeps maintenance out of the read-only Provider contract and uses the existing typed transaction boundary for mutations. Capability fields are explicit per operation; developer providers do not pretend to support fuzzy search, cleanup, or authoritative simulation where their CLIs do not expose those safely.

The `Provider` trait exposes the safe read surface:

- `source_info`;
- `search`;
- `info`; and
- `diagnostic`.

The separate `TransactionProvider` trait exposes only plan, typed-operation, and post-operation verification methods. A provider can implement those methods only when it can represent the operation safely; the UI does not assume that every ecosystem has identical capabilities. The current providers expose single-package install/remove capability, while future update, cleanup, and batch operations remain outside this interface.

`ProviderRegistry` creates the supported providers, selects one source when requested, or queries all providers when no source is specified. Independent blocking searches and update inventories use bounded standard-thread concurrency and are sorted after collection for deterministic output. A failure from one provider becomes a structured issue and does not discard results from the others. Providers whose exact existence resolution is incomplete are source-qualified for mutation rather than treated as negative matches.

## Normalized data

`Package` contains provider-neutral fields such as source, canonical provider ID, display name, version, summary, description, installed state, classification, origin, architecture, homepage, license, and size. Optional fields are omitted from JSON when the provider cannot establish them. Provider-specific values remain in a metadata map instead of leaking into every shared field.

Provider-qualified references use the concise form `apt:curl`, `flatpak:org.example.App`, `snap:firefox`, `cargo:ripgrep`, `npm:@scope/package`, `pnpm:typescript`, `uv:ruff`, or `pipx:black`. A colon is interpreted as a source qualifier only when its prefix is a known source, so Debian architecture names such as `libssl:amd64` are not accidentally rewritten.

## Read-only provider decisions

APT uses `apt-cache` for package search and records, with `--no-generate`, and `dpkg-query` for installed state. This is intentionally separate from interactive terminal output. Nala is detected and shown as an available APT frontend, but APT remains the normalized source identity. `orbis update` reads the current local index; `orbis refresh` is the explicit metadata-refresh action.

Flatpak uses the documented `--columns` forms where possible. Search and installed listing are parsed as column records, preserving application IDs, friendly names, versions, branches, remotes, architecture, and descriptions.

Snap uses its stable command-line surfaces: `snap find` for discovery, `snap list` for installed state, and `snap info` for detailed metadata. Its tabular search output is parsed by column position and its indented info record is parsed without treating the output as YAML.

These choices follow the providers' official command references and avoid scraping ANSI presentation. Developer providers that do not maintain a separate local catalog report a successful metadata-only refresh without inventing a provider command.

## Process safety

Providers receive a `CommandRunner`. The production implementation uses `std::process::Command` with structured argument vectors, null stdin, captured stdout/stderr, and bounded timeouts. It never assembles a shell command and never calls `sh -c`.

The test seam accepts a fake runner, allowing parser, aggregation, and unavailable-provider tests to run without executing any package manager. The process module also captures non-zero status and retains technical detail for JSON or debugging without exposing it in normal prose.

Timeout policy is part of the process boundary: metadata and planning calls are bounded, while an active typed package mutation has no generic automatic kill timeout. Provider-aware cancellation is intentionally deferred.

The transaction planner produces a typed `ProviderOperation`, never an arbitrary program and argument string. The production privilege boundary performs a narrow `sudo -v` authorization followed by an exact non-interactive typed command. It does not accept shell input, handle passwords, or expose `run_as_root(program, args)`.

APT planning invokes `apt-get -s -o Debug::NoLocking=true` with `LC_ALL=C` and `DEBIAN_FRONTEND=noninteractive` scoped to that process. The parser normalizes install/remove/configure lines and blocks plans that report additional removals. Flatpak planning uses read-only `remote-info` and scoped installed listings; it is marked partial because runtimes and extensions may be resolved at commit. Snap planning uses `snap info`, defaults to latest/stable when no channel is supplied, and is marked partial because Snap has no equivalent no-action impact simulation.

Provider execution is followed by a scoped installed-state check. Results distinguish succeeded, partially verified, and failed. After confirmation, the core writes an `executing` record before invoking the provider operation, then atomically replaces the same operation ID with the final sanitized result under `$XDG_STATE_HOME/orbis/transactions`, falling back to `$HOME/.local/state/orbis/transactions`.

## Orbis Brief

Maintenance uses the same typed operation executor and privilege boundary. A coordinated MaintenancePlan contains independent provider plans and is explicitly non-atomic. APT refresh, safe upgrade, and autoremove use fixed maintenance operation variants; Flatpak AppStream refresh is scoped, Flatpak upgrade is blocked when its documented side effects cannot be planned safely, and Snap refresh checks use read-only refresh-list commands. Upgrade plans are revalidated before execution.

`PackageBrief` combines a normalized package with:

- a plain-language headline;
- explanatory paragraphs;
- optional direct-use guidance;
- optional examples;
- caution/context;
- a confidence label; and
- provenance entries.

Milestone 1 uses a small maintained knowledge mechanism for a few high-confidence examples such as `btop`, `ffmpeg`, and `libssl-dev`, plus conservative classifications based on metadata and package naming. Unknown packages fall back to provider text and explicitly say that Orbis has not inferred a richer explanation. There is no hosted service or external API dependency.

The evidence structure leaves room for richer local metadata sources later without changing the CLI or package model.

## Rendering and automation

The core never emits ANSI or terminal decoration. `render/theme.rs` owns semantic tokens for identity, hierarchy, state, provider badges, risk, and surfaces. It adapts plain output and Ratatui styles to truecolor, 256-color, or basic terminals, and disables styling for `NO_COLOR`, `--no-color`, or monochrome operation. Piped output remains plain, and `TERM=dumb` selects ASCII markers.

`orbis` launches the TUI only when stdin and stdout are terminals and `TERM` is not `dumb`. `orbis dashboard`/`orbis ui` can be explicit, while `--plain` is an escape hatch. JSON always bypasses the TUI. `ratatui::run` owns the Crossterm raw-mode/alternate-screen lifecycle and restores the terminal on normal exit or returned initialization/draw errors. The event loop handles Ctrl-C, Escape/back, resize through Ratatui's current frame area, and a minimum-size message instead of drawing off-screen.

The dashboard creates bounded, read-only standard-thread workers for source snapshots, update inventories, searches, and plans. They communicate through a channel; the event loop never waits on provider I/O. Package mutations are not background work: a selected action first requests an `OperationPlan`, displays it in a review overlay, rejects blocked plans, and only then calls the same typed executor and history lifecycle as the plain CLI. Explicit interactive commands use the same review and live execution view. Maintenance source activity is carried by typed progress events; provider output is bounded and presentation-only. Update review is likewise a read-only maintenance plan.

`--json` changes the stdout contract to structured data for the home view, `sources`, `search`, `info`, `explain`, `health`/`doctor`, and transaction plans/results. Decorative messages are not mixed into JSON stdout. Human-readable provider issues remain available as structured fields. Mutation commands refuse non-interactive execution unless `--yes` is supplied; `--plan` and `--dry-run` never cross the execution boundary. `update` is the beginner read-only update check; `refresh` is the explicit metadata-refresh action, while `upgrade` remains the compatibility spelling for applying an update plan.

## Safety scope

Milestones 3 and 4 add update inventories, coordinated upgrade and cleanup plans, history queries, provider-specific explanation evidence, and user-wide developer-tool coverage. Flatpak remote configuration, Snap retention, package indexes, developer-tool configuration, project manifests, and package-manager cleanup are never changed by planning. Privilege is requested only after an exact administrator-scoped maintenance plan is confirmed; Cargo, npm, pnpm, uv, and pipx operations never request it.
