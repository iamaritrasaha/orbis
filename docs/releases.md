# Orbis release process

Orbis uses `dist` (formerly `cargo-dist`) for reproducible binary distribution. The repository pins the release-tool version in [dist-workspace.toml](../dist-workspace.toml); Beta 1 uses `dist` 0.32.0.

## Beta 1 scope

The `v0.1.0-beta.1` release is a Linux-first early public release. It produces GNU/Linux archives for:

- `x86_64-unknown-linux-gnu`, built on `ubuntu-22.04`;
- `aarch64-unknown-linux-gnu`, built on GitHub's native `ubuntu-22.04-arm` runner.

macOS, Windows, musl Linux, and crates.io publication are intentionally outside this beta. A successful build proves that an artifact was built; it does not by itself prove runtime acceptance on that architecture. The maintainer's local runtime checks are performed on x86_64.

Each binary archive contains the binary, `README.md`, `LICENSE`, `CHANGELOG.md`, `orbis.1`, and shell completion files. The release also contains per-archive SHA-256 files, a unified checksum, the generated shell installer, and a machine-readable `dist-manifest.json` retained as workflow evidence. A separate source checkout tarball is disabled.

The shell installer installs only to a user-owned location (`~/.local/bin` by default). It does not need `sudo`; Orbis may later request administrator authorization for a system package transaction, which is a separate operation.

## Pre-tag validation

Run these checks from a clean checkout on the release commit:

~~~sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo package --workspace --locked
git diff --check
dist plan --output-format=json --no-local-paths
dist build --tag=v0.1.0-beta.1 --artifacts=local --target=x86_64-unknown-linux-gnu --output-format=json
dist build --tag=v0.1.0-beta.1 --artifacts=global --output-format=json
~~~

Inspect the generated manifest before publishing. Confirm the tag is `v0.1.0-beta.1`, the announcement is marked prerelease, only the two supported Linux targets are present, and each archive lists the expected release files. For every locally built archive, run `tar -tJf` and `sha256sum --check` against its `.sha256` file. Run `sh -n` against the generated installer.

The global build needs the local artifacts from every configured target to populate the unified checksum. The release workflow supplies those artifacts from its native target jobs; a local x86_64-only build may therefore produce an empty or incomplete local `sha256.sum` while still validating installer generation.

## Publishing gate

The generated workflow runs `dist plan` for pull requests and creates the GitHub Release only for a pushed version tag. The final release sequence is:

1. land the validated release commit on `main`;
2. confirm the release workflow and CI are green;
3. create and push the exact tag `v0.1.0-beta.1`;
4. wait for both Linux artifact jobs, the global installer/manifest job, attestations, and the host job to complete;
5. inspect the prerelease assets and verify an extracted binary with `orbis --version`.

Do not push the tag while a prerequisite is failing. Do not describe ARM64 as runtime-tested unless a real ARM64 runtime check has been performed.

## Workflow permissions

The generated release workflow has repository `contents: write` permission because its tag path must create and upload the GitHub Release. The target build job overrides this with `contents: read` plus the narrowly required `attestations: write` and `id-token: write` permissions for GitHub Artifact Attestations. No package publication permission is configured.
