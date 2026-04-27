---
name: integration-glue
description: |
  Wiring artifacts produced by upstream tasks into a runnable system: CI
  configs, build scripts, environment files, and dependency declarations.
compatible_roles: [integrator, implementer]
---

# Integration glue playbook

Integration is the work that turns "code that compiles in isolation" into
"code that runs in the actual system". It is rarely glamorous; it is always
where bugs hide.

## The 4 axes of glue

1. **Build / package**: dependency files (`Cargo.toml`, `package.json`,
   `pyproject.toml`), build scripts, lockfile commits.
2. **Configuration**: `.env.example`, default config JSON/TOML, secret
   placeholders. NEVER commit real secrets.
3. **CI**: lint / test / build steps; cache keys; required check names.
4. **Runtime wiring**: dependency injection container, route registration,
   feature flag toggles.

## Discipline

- **Lockfiles are part of the change**. If you bump a dependency, commit the
  updated lockfile in the same change. Never let the lockfile drift in CI.
- **Default config must work out of the box**. A new clone + standard
  `setup` script must boot the app without manual edits.
- **CI runs the same commands as local**. If `pnpm test` works locally but
  CI runs `npm test`, you have a divergence bug waiting to happen.
- **Deletions are integration changes too**. If you remove a feature, also
  remove its CI step, env var, and dependency.

## Common traps

- Adding a dep without checking license compatibility (the project is
  GPL-3.0; copy-left compatible deps only).
- Pinning a major version in `Cargo.toml` but letting `package.json` track
  `^x.y` — the JS dep silently major-bumps in CI.
- Hard-coding a path that works on your machine: always use repo-relative or
  `$HOME` references.
