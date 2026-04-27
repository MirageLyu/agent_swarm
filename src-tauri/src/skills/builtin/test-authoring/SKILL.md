---
name: test-authoring
description: |
  How to design and write tests that catch real regressions: pick the right
  level (unit vs integration), write assertion-rich tests, avoid brittle
  fixtures.
compatible_roles: [tester, implementer]
---

# Test authoring playbook

## Pick the right level

- **Unit test** the behavior of a function with no I/O. Mock nothing if you
  can avoid it; if you must mock, the design probably wants a different seam.
- **Integration test** when the value is in the cooperation between modules,
  the I/O contract, or the database/HTTP boundary. Use real dependencies
  whenever feasible (in-memory SQLite, ephemeral tempdir).
- **End-to-end test** sparingly; they are slow and flaky. Reserve for the
  one or two flows that absolutely must not break.

## Test naming

`test_<unit>_<scenario>_<expected>`:

- `test_login_with_wrong_password_returns_unauthorized`
- `test_dispatch_when_no_ready_tasks_does_nothing`

Don't write `test_login_1`. The next reader cannot tell what broke when it
fails.

## Assertions

- One **logical** assertion per test, not necessarily one `assert!()`.
  Multiple `assert!()` calls verifying the same fact are fine.
- Compare against expected **values**, not against the function output:
  `assert_eq!(result.status, Status::Ok)` is better than
  `assert!(result.is_ok())` if the type carries information.
- For collections: prefer `assert_eq!(actual, expected_vec)` over
  `assert_eq!(actual.len(), N)`.

## Fixtures

- Build small, named factory functions: `fn user_with_role(role: &str) ->
  User`. Avoid 50-line setup blocks copied across tests.
- A test that needs 20 lines of setup is telling you the design is too
  coupled. Listen.

## Coverage

- Coverage is a debugging tool, not a goal. 80% line coverage with shallow
  asserts is worse than 50% with deep asserts.
- The branches that are hardest to test are usually the ones most worth
  testing.
