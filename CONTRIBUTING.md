# Contributing

Issues and pull requests are welcome.

- **Start from the requirements.** `docs/spec.md` says what throng must do. A change in behaviour
  updates it in the same pull request, and says what it replaces if it changes an existing
  requirement.
- **Test at the cheapest layer that proves the change.** Domain rules get unit tests in their crate.
  Terminal behaviour gets a daemon test against a real shell. What a user sees gets a UI test in
  `crates/throng-app/tests/ui.rs`, which drives the app through its accessibility tree.
- **Run the checks before pushing:**

  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

  CI runs the same checks on Linux, macOS and Windows, and a pull request needs all three green.
  No test run may leave a `throng daemon` or shell process behind.

By contributing you agree that your contribution is licensed under the GNU Affero General Public
License v3.0 only, the licence of this project. There is no separate contributor agreement.
