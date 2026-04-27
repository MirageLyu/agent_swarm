---
name: code-implementation
description: |
  General coding hygiene that applies to every implementer task: naming,
  error handling, logging, and incremental compilation discipline.
compatible_roles: [implementer, integrator]
---

# Implementation playbook

## Naming

- **Identifiers describe the role, not the type**. `user_repo` is better than
  `user_obj`. `pending_review_count` is better than `int_2`.
- **Don't repeat the namespace**. In `mod auth`, prefer `fn login` to
  `fn auth_login`.
- **Booleans assert a fact**: `is_signed_in`, `has_pending_changes`,
  `should_retry`. Avoid `flag`, `status_bool`.

## Error handling

- **Don't `unwrap()` in production code paths**. Map errors with context:
  `.map_err(|e| anyhow!("failed to load X: {e}"))`.
- **Surface, don't swallow**: a caught error must either be logged at WARN+
  with context, or transformed into a domain error returned to the caller.
- **Fail loudly at the boundary**: input validation should reject early at the
  IPC / HTTP / CLI boundary. Internal functions can assume validated input.

## Logging

- **One log line per side-effect**: file write, network call, DB write, spawn.
- **Use structured fields** (`tracing::info!(user_id, action = "login")`)
  instead of pre-formatted strings.
- **DEBUG = developer**, **INFO = operator**, **WARN = something to check**,
  **ERROR = something failed**. Don't log "starting function X" at INFO.

## Incremental compilation

- After every meaningful edit, run the smallest verification you can:
  - Rust: `cargo check -p <package>`
  - TypeScript: `pnpm tsc --noEmit`
  - Python: `mypy <module>` or `python -c "import <module>"`
- Don't accumulate 10 files of changes before checking. Each broken
  intermediate state grows the debugging cost super-linearly.
