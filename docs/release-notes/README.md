# Curated release notes

Orbis supports optional hand-written release notes that override the
auto-generated cargo-dist release body for a specific tag.

## How it works

The release workflow checks for a file at:

```
docs/release-notes/<TAG>.md
```

- If the file **exists**, its first H1 line becomes the GitHub Release title
  and the remaining content becomes the release body.
- If the file **does not exist**, the workflow falls back automatically to
  cargo-dist's generated `announcement_title` and `announcement_github_body`.
  The release will publish normally without any curated notes.

## To curate a release

1. Create `docs/release-notes/vX.Y.Z.md` (e.g. `v0.1.0-beta.2.md`).
2. The **first line** must be a Markdown H1:
   ```
   # <GitHub Release title>
   ```
3. The remainder of the file is the release body (standard GitHub Markdown).
4. Commit the file to `main` before pushing the release tag.

The workflow validates that the first line starts with `# ` and will fail the
curated-note path with a clear error if it does not, leaving the cargo-dist
fallback unaffected.
