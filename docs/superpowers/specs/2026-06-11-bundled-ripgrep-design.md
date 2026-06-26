# Bundled ripgrep Design

Date: 2026-06-11

## Goal

Miragenty's `grep` / `search_files` tools should use real ripgrep semantics without requiring users to install `rg` globally. The app should ship a pinned `rg` binary for release builds, while keeping the implementation ready to add Linux and Windows packages later.

## Decision

Use official ripgrep release binaries as bundled Tauri resources. Do not commit downloaded `rg` binaries to git. Commit only a manifest and fetch script that can download, verify, and stage the correct binary before building.

Runtime lookup order:

1. Bundled ripgrep resource for the current platform.
2. PATH `rg` fallback for development or incomplete local staging.
3. Existing structured dependency error if neither is available.

The bundled binary remains the release path, so packaged app behavior is stable and uses actual `rg` rather than a partial Rust reimplementation.

## Repository Layout

Add:

```text
scripts/fetch-rg.mjs
src-tauri/vendor/rg/manifest.json
src-tauri/vendor/rg/README.md
```

Downloaded files are staged under:

```text
src-tauri/vendor/rg/bin/<target>/rg
src-tauri/vendor/rg/bin/<target>/rg.exe
```

`src-tauri/vendor/rg/bin/` is ignored by git.

The manifest records ripgrep version, target triples, release URLs, SHA-256 checksums, archive type, and the executable path inside each archive. Initial support targets macOS, with the data model ready for Linux and Windows.

## Build and Bundle Flow

`pnpm bundle` should run the fetch script before `tauri build`. The script:

1. Detects the host target.
2. Reads the manifest.
3. Downloads the pinned official ripgrep archive if the staged binary is missing.
4. Verifies SHA-256 before extraction.
5. Extracts only the `rg` executable into `src-tauri/vendor/rg/bin/<target>/`.
6. Preserves executable permissions on Unix.

`tauri.conf.json` includes the staged binary as a resource. For macOS, the staged binary should be copied into the app bundle. The exact resource mapping should be implemented against Tauri v2's supported `bundle.resources` syntax and verified by resolving the file through Tauri's resource directory API.

## Runtime Architecture

Keep `ToolExecutor` responsible for constructing ripgrep arguments, preserving the prior CLI behavior:

- `--files-with-matches`
- `--count-matches`
- `--line-number`
- `-C`, `-A`, `-B`
- `-i`
- `--multiline`
- `--multiline-dotall`
- repeated `--glob`
- `--type`
- `--color=never`
- `-e <pattern>`

Replace hard-coded `Command::new("rg")` with a resolver that returns a command path. The resolver should prefer the bundled resource path when available and executable. If it cannot resolve the bundled resource, it should fall back to `rg` on PATH.

Remove the new `grep_engine.rs` module and the `grep`, `grep-regex`, `grep-searcher`, and `ignore` dependencies that were added for the partial Rust implementation.

## Error Handling

If the bundled binary is missing and PATH fallback also fails, return the existing capability/dependency error. Do not silently switch to a partial Rust grep engine, because that would reintroduce semantic drift.

If the bundled binary exists but cannot launch, report a structured capability error that includes the attempted path and says the app should fall back to PATH only if available.

## Testing

Tests should validate the real `rg` path. The test helper should prefer the staged vendor binary if present, otherwise use PATH `rg`. Tests that assert grep/search_files behavior should no longer target the partial Rust engine.

Key checks:

- `grep` and `search_files` alias output remains identical.
- glob filters and default internal-directory exclusions are passed as repeated real `rg --glob` flags.
- count/content/files-with-matches modes match `rg` behavior.
- missing `rg` produces the expected dependency error.
- fetch script verifies SHA-256 and stages the binary under the expected target directory.

## Non-goals

- Do not implement a custom Rust grep engine.
- Do not commit large ripgrep binaries to git.
- Do not prioritize PATH `rg` over the bundled binary in release behavior.
- Do not add Linux/Windows binaries in the first implementation unless needed immediately; keep the manifest shape ready for them.
