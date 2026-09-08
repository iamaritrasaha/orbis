# Developer providers

Milestone 4 adds user-wide developer tools while keeping project dependency graphs out of Orbis.

| Source | Orbis manages | Not managed |
| --- | --- | --- |
| Cargo | `cargo install` binary crates | `Cargo.toml`, `Cargo.lock`, `--git`, `--path`, and arbitrary registries/sources |
| npm | global npm packages | project `package.json`, lockfiles, and `node_modules` |
| pnpm | global pnpm packages | project/workspace manifests, lockfiles, and `node_modules` |
| uv | persistent `uv tool` environments | uv projects, `pyproject.toml`, `uv.lock`, `.venv`, and `uv pip` environments |
| pipx | current-user pipx applications | arbitrary Python environments and `pipx --global` |

Use canonical qualified names such as `cargo:ripgrep`, `npm:typescript`, `npm:@scope/package`, `pnpm:eslint`, `uv:ruff`, and `pipx:black`. Registry package names only are accepted. Paths, Git URLs, tarballs, local scripts, aliases, shell-like input, and arbitrary provider flags are rejected.

Developer operations never use sudo. Orbis checks the active Cargo root, npm prefix, pnpm global environment, uv tool bin directory, or pipx user bin directory. A system-owned or unusable destination blocks mutation with a remediation hint; Orbis does not rewrite package-manager configuration, run `pnpm setup`, run `pipx ensurepath`, or modify shell files.

Cargo search/info use Cargo's stable registry commands. Cargo install planning is partial because stable Cargo's dry-run is still unstable. Installed Cargo provenance is not reliably exposed by the supported `cargo install --list` interface, so Orbis never guesses a crates.io replacement during automatic upgrades.

npm uses machine-readable global list/search/view/outdated commands. Its supported global dry run is used for exact install/remove planning. Lifecycle scripts are not hidden or disabled. Global updates are exact reviewed candidates; if the installed version is newer than npm's latest dist-tag, Orbis excludes it rather than downgrading it.

pnpm uses its global JSON interfaces and preserves the user's build-script approval configuration. There is no invented global dry run. If the global bin directory is missing from PATH, Orbis diagnoses the issue and does not silently run `pnpm setup`.

uv uses `uv tool list --outdated` and exact `uv tool upgrade` operations. uv itself preserves the version constraints and settings recorded for a tool; Orbis does not replace them with an absolute-latest request. uv and pipx do not provide fuzzy registry search in the provider contract, so search simply omits them.

pipx uses its structured installed snapshot and `--skip-maintenance` for exact operations where supported. This prevents unrelated shared-library maintenance during an Orbis single-package transaction. Pinned applications are shown as held and are never silently upgraded. Existing recorded pipx backends are preserved.

`orbis clean` remains conservative. It does not run `cargo clean`, `npm cache clean --force`, `pnpm store prune`, `uv cache clean`, or `pipx cache purge`; cache management is a separate future feature. `orbis update` reports that these registries are queried live rather than inventing a catalog mutation. Use `orbis refresh` when the goal is to refresh software information.
