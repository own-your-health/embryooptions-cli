# Contributing

1. `make verify` must pass: fmt-check, clippy with warnings denied, the
   tests, the smoke test.
2. Test data is synthetic: an answer's shape becomes test data with the
   real answer's structure and dummy values, never a real capture, and
   tokens are built from synthetic claims (`token::test_token`).
3. No personal data anywhere: code, docs, commit messages, pull requests,
   issues. `tests/no_personal_data.rs` enforces the shapes; review the rest.
4. An API change is observed signed in first and recorded in
   `docs/api.md`, traps included; what is only read from the web app's
   JavaScript stays marked *bundle*.
5. Drive the installed binary on synthetic data before calling a change
   done: `make install`, then every command and error path against a
   loopback server (AGENTS.md).
6. Squash merges only, with an approval and green CI.
