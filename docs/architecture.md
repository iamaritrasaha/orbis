# Orbis architecture

Orbis is split into a small library crate and a binary crate:

~~~text
orbis-cli
  command parsing
  terminal renderer
  JSON/stdout policy
        |
orbis-core
  discovery and resolution
  normalized models
  deterministic explanations
  diagnostics
  provider contract
  shell-free process runner
        |
  APT/Nala frontend | Flatpak | Snap
~~~

## Provider boundary

The `Provider` trait exposes only the operations needed by the current milestone:

- `source_info`;
- `search`;
- `info`; and
- `diagnostic`.

The normalized `ProviderCapabilities` model already has explicit fields for installed-state and future mutation support. Mutation is false for every provider today. A provider can later add an operation only when it can implement that operation safely; the UI does not assume that every ecosystem has identical capabilities.

`ProviderRegistry` creates the supported providers, selects one source when requested, or queries all providers when no source is specified. A failure from one provider becomes a structured issue and does not discard results from the others.

## Normalized data

`Package` contains provider-neutral fields such as source, canonical provider ID, display name, version, summary, description, installed state, classification, origin, architecture, homepage, license, and size. Optional fields are omitted from JSON when the provider cannot establish them. Provider-specific values remain in a metadata map instead of leaking into every shared field.

Provider-qualified references use the concise form `apt:curl`, `flatpak:org.example.App`, or `snap:firefox`. A colon is interpreted as a source qualifier only when its prefix is a known source, so Debian architecture names such as `libssl:amd64` are not accidentally rewritten.

## Read-only provider decisions

APT uses `apt-cache` for package search and records, with `--no-generate`, and `dpkg-query` for installed state. This is intentionally separate from interactive terminal output. Nala is detected and shown as an available APT frontend, but APT remains the normalized source identity.

Flatpak uses the documented `--columns` forms where possible. Search and installed listing are parsed as column records, preserving application IDs, friendly names, versions, branches, remotes, architecture, and descriptions.

Snap uses its stable command-line surfaces: `snap find` for discovery, `snap list` for installed state, and `snap info` for detailed metadata. Its tabular search output is parsed by column position and its indented info record is parsed without treating the output as YAML.

These choices follow the providers' official command references and avoid scraping ANSI presentation.

## Process safety

Providers receive a `CommandRunner`. The production implementation uses `std::process::Command` with structured argument vectors, null stdin, captured stdout/stderr, and bounded timeouts. It never assembles a shell command and never calls `sh -c`.

The test seam accepts a fake runner, allowing parser, aggregation, and unavailable-provider tests to run without executing any package manager. The process module also captures non-zero status and retains technical detail for JSON or debugging without exposing it in normal prose.

The future privileged-operation seam should be separate from this read-only runner. It must not grow into a general sudo string builder. A future operation planner should produce a typed provider operation, show a provider-owned dry-run/preview where available, require explicit user confirmation, and delegate escalation to a narrow audited mechanism.

## Orbis Brief

`PackageBrief` combines a normalized package with:

- a plain-language headline;
- explanatory paragraphs;
- optional direct-use guidance;
- optional examples;
- caution/context;
- a confidence label; and
- provenance entries.

Milestone 1 uses a small maintained knowledge mechanism for a few high-confidence examples such as `btop`, `ffmpeg`, and `libssl-dev`, plus conservative classifications based on metadata and package naming. Unknown packages fall back to provider text and explicitly say that Orbis has not inferred a richer explanation. There is no hosted model or external API dependency.

The evidence structure leaves room for richer local metadata sources later without changing the CLI or package model.

## Rendering and automation

The core never emits ANSI or terminal decoration. The CLI renderer owns color, Unicode fallback, wrapping, and spacing. ANSI is enabled only for a TTY and is disabled by `NO_COLOR` or `--no-color`. Piped output remains plain, and the renderer uses simple ASCII markers when `TERM=dumb`.

`--json` changes the stdout contract to structured data for `sources`, `search`, `info`, `explain`, `doctor`, and the home view. Decorative messages are not mixed into JSON stdout. Human-readable provider issues remain available as structured fields.

## Safety scope

There is no install, remove, update, upgrade, autoremove, cleanup, rollback, history database, daemon, telemetry, or background service in Milestone 1. Diagnostics are also read-only. This scope is intentional: the first release proves provider isolation, normalization, explanation, and trustworthy presentation before machine-altering behavior is designed.
