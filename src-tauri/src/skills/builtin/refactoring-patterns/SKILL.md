---
name: refactoring-patterns
description: |
  A short catalog of refactorings safe to perform without external behavior
  changes, and the verification step that goes with each.
compatible_roles: [refactorer, implementer]
---

# Refactoring catalog

The cardinal rule: **a refactor is only complete when the test suite that
covered the original code still passes unchanged**. If you have to update
tests to make them pass, you changed behavior — that is a feature, not a
refactor.

## Safe refactorings

| Refactoring          | When it's safe                                | Verification |
|----------------------|-----------------------------------------------|--------------|
| Rename               | All call sites are visible (no reflection).   | Compiler / type checker. |
| Extract function     | Body has a single clear purpose, ≥ 5 lines.   | Existing tests pass. |
| Inline function      | Function is called ≤ 2 times and trivial.     | Existing tests pass. |
| Move function        | Target module already imports the dependencies. | Build + tests. |
| Replace conditional with polymorphism | Each branch maps cleanly to a type. | Existing tests pass. |
| Introduce parameter object | Function has 4+ related parameters.       | Build + tests. |

## Unsafe (require new tests)

- Changing return types from `T` to `Result<T, E>`: caller must handle errors.
- Replacing sync with async: caller's runtime must support it.
- Splitting a class/module into two: state ownership must be re-validated.
- Replacing an inheritance hierarchy with composition.

## Process

1. Snapshot the test suite output. Save it.
2. Apply ONE refactoring. Re-run tests.
3. If green, commit. If red, revert; do not "patch up" the test.
4. Repeat. A refactoring PR with 20 commits, each green, is much safer to
   review than 1 commit with 200 file changes.
