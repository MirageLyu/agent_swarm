# Bundled ripgrep

Miragenty uses the official `rg` binary for `grep` / `search_files` so agent search behavior matches ripgrep CLI semantics.

The downloaded binaries are not committed to git. They are staged by:

```bash
node scripts/fetch-rg.mjs
```

The script reads `manifest.json`, downloads the pinned official ripgrep release for the current host target, verifies SHA-256, and writes the executable to `bin/<target>/rg`.

Tauri packages the staged `bin/**` files as app resources. Runtime lookup prefers the bundled or staged binary, then falls back to `rg` on PATH for development environments.
