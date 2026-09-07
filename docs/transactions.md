# Orbis transactions

Milestone 2 deliberately limits mutation to one exact package and one provider per invocation. The transaction path is:

~~~text
CLI input
  -> validated PackageRef
  -> strict provider resolution
  -> provider-owned OperationPlan (risk, confidence, completeness)
  -> rendered plan and confirmation
  -> typed ProviderOperation
  -> narrow privilege executor
  -> provider-scoped verification
  -> sanitized XDG history record
~~~

## Planning guarantees

`--plan` and `--dry-run` stop before the executor. They do not refresh indexes, add Flatpak remotes, refresh Snap metadata, or download/deploy a package.

APT uses `apt-get -s` with `Debug::NoLocking=true`, `LC_ALL=C`, and a noninteractive frontend environment. Its documented `Inst`, `Remv`, and `Conf` lines are normalized into changes. If the simulation reports additional removals, the Orbis plan is blocked.

Flatpak uses scoped installed listings and `remote-info --show-details`. Flatpak does not provide a complete no-action dependency simulation through this path, so runtime/extension impact is marked partial and any known runtime is shown as a provider-reported change. Orbis never uses `--no-deploy` as a substitute for a no-mutation plan.

Snap uses `snap info` and exact installed-state inspection. Snap plans are partial because the CLI does not expose an equivalent zero-action impact simulation. An omitted channel means the provider's normal latest/stable selection. Removal intentionally omits `--purge`, so Snap's normal retained-data snapshot behavior remains intact.

Cargo, pnpm, uv, and pipx use truthful partial metadata plans; npm uses the provider's supported global dry run when it succeeds. No developer-provider plan uses sudo.

Read-only queries and planning commands use bounded timeouts. The real provider mutation command has no generic wall-clock timeout: Orbis allows APT, Flatpak, or Snap to complete normally and does not automatically kill an active package transaction. Only the separate `sudo -v` authorization check remains bounded.

## Confirmation and privilege

Ambiguous resolution, invalid identifiers, unknown Flatpak remotes, incomplete plans, and blocked APT impact never reach confirmation. Noninteractive execution without `--yes` is refused. `--yes` only skips the Orbis prompt for the already resolved plan; it does not turn off provider safety checks.

System operations are represented by a closed `ProviderOperation` enum. The production executor authorizes with `sudo -v`, then invokes only the corresponding fixed provider executable with structured arguments and `sudo -n`. User-scoped Flatpak operations do not request administrator authorization. Orbis does not accept passwords, concatenate shell commands, or expose a generic root execution API.

## Records

After confirmation and immediately before invoking the typed provider operation, Orbis writes one JSON file with lifecycle `executing` under:

~~~text
$XDG_STATE_HOME/orbis/transactions/
~~~

or, when `XDG_STATE_HOME` is not set:

~~~text
$HOME/.local/state/orbis/transactions/
~~~

The same operation ID is atomically replaced after execution and verification with lifecycle `succeeded`, `partially_verified`, or `failed`. Records contain the request, resolved plan, exit status, lifecycle, and verification status. They do not contain raw stdout/stderr, command-line dumps, passwords, or tokens. A temporary file and rename provide atomic replacement for each record. Legacy Milestone 2 records remain readable through schema defaults and lifecycle inference from their final result.

## Scope exclusions

Milestones 3 and 4 add a separate maintenance path rather than folding system-wide or developer-tool maintenance into the single-package operation path. It provides normalized update inventories, coordinated non-atomic maintenance plans, revalidation, conservative APT autoremove planning, history queries, and provider-specific explanation evidence. Flatpak cleanup remains unsupported where exact non-mutating planning is unavailable, Snap retention is never changed by Orbis, and Cargo upgrade provenance remains incomplete when the original source cannot be proven.

Single-package transactions still do not implement full-upgrade, rollback, undo, or arbitrary batch operations. Milestone 3 maintenance has its own plan models and safety review rather than being folded into the single-package operation path.

## Primary references

- [Debian apt-get reference](https://manpages.debian.org/unstable/apt/apt-get.8.en.html)
- [Flatpak command reference](https://docs.flatpak.org/en/latest/flatpak-command-reference.html)
- [Snap getting started](https://snapcraft.io/docs/tutorials/get-started/)
- [Snap channels and tracks](https://snapcraft.io/docs/explanation/how-snaps-work/channels-and-tracks/)
- [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/0.8/)
