# Contributing to Orbis

Orbis is a small, safety-first Rust CLI. The most useful contributions preserve clear boundaries and make provider behavior easier to understand.

## Before opening a change

Read [docs/architecture.md](docs/architecture.md). In particular:

- keep provider-specific parsing inside its provider module;
- use the injected command runner instead of calling 'std::process::Command' from a provider;
- keep package mutations out of Milestone 1;
- represent unavailable or unknown metadata honestly; and
- add fixture tests when a provider output shape changes.

## Local checks

~~~sh
cargo fmt --all
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
~~~

Provider integration checks should be read-only and clearly separated from fixture tests. Do not add a test that needs sudo, changes package databases, changes remotes, or depends on a graphical desktop.

## Pull requests

Describe:

- the user-visible behavior;
- the providers and command formats affected;
- the tests and local commands actually run; and
- any provider limitation or unavailable backend observed.

Keep the public identity focused on Orbis. Do not add hosted-service, model, or agent branding to user-facing output or documentation.
