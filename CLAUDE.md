# throng — working notes

throng is a Rust workspace: one `throng` binary (`crates/throng-app`) that draws its UI with egui and
runs its terminals in a detached daemon (`throng daemon`, the same binary). What it must do is
`docs/spec.md`; its principles (I–XI) are the ones code comments cite. The README has the crate map.

Toolchain: stable (`rust-toolchain.toml`), edition 2024, MSRV 1.95. `unsafe` is denied workspace-wide
and allowed only where an OS call needs it, with a `// SAFETY:` comment.

## Verifying done-ness

**Done means `.github/workflows/ci.yml` green on Linux, macOS and Windows on the pushed SHA.** A
green local run is progress, not done-ness: macOS and Windows differ in ways Linux cannot show
(`/private/var` aliases, Cmd for Ctrl, ConPTY, job objects). Quote the run URL and the SHA when
reporting done, and treat a green run as stale the moment anything changes after it.

Locally, from the repository root, before every push:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
pgrep -fa "[t]hrong daemon"      # must print nothing
```

The UI tests (`crates/throng-app/tests/ui.rs`) and daemon tests start real `throng daemon` processes
and real shells. A daemon, shell or child left behind after a run is a defect, not noise. When
killing strays by hand, use `pgrep -f "[t]hrong daemon" | xargs -r kill`: a bare `pkill -f` pattern
matches the shell running it.

A change to `packaging/` or the Package workflow is done when `.github/workflows/package.yml` is
green too: it builds each package and runs it from where a user would.

## Seeing the app without a display

On a Linux container with no display, render the real app to a file:

```sh
cargo run -p throng-app --example seed -- /tmp/demo-home "$PWD"
THRONG_HOME=/tmp/demo-home THRONG_SCREENSHOT=/tmp/shot.png \
  xvfb-run -a env LIBGL_ALWAYS_SOFTWARE=1 cargo run -p throng-app
```

winit needs `libxkbcommon-x11-0`. `THRONG_SEED_SUB=1` makes the seed include a sub-workspace.

## Formatting

`cargo fmt` is the only formatter (`rustfmt.toml`: `max_width = 110`). rustfmt does not rewrap
comments, so keep them under the width by hand.

## Rules this codebase has learned

- **One condition, one notice.** A single condition raises a single notice, keyed by the condition,
  and the actions that resolve it live on that notice. A second caller reporting the same state makes
  the first notice louder rather than raising another. Say what is wrong, not what the user may not
  do. Anything with an action attached is inline, not a toast.
- **Find the requirement that already governs a behaviour before writing a new one.** Search
  `docs/spec.md` and the tests first (`git grep -n "<the observable>" -- crates`). Behaviour that
  looks like an oversight is often a decision: settings keep unknown keys but drop retired ones, and
  both are deliberate. A requirement that should change is changed in `docs/spec.md` in the same
  commit, saying what it replaces and why, never contradicted silently.
- **Platform rules are values, not `cfg!`s scattered through the domain** (Principle II). Path
  identity, file-name validity and process-tree questions go through `throng-platform`, so the
  domain is testable on one OS with another's rules.
- **Terminal keys belong to the terminal** (Principle IV). A new command bound to a reserved chord
  (Ctrl+C, D, Z, A, E, W, U, K, R, L, Q) where terminals are live is a defect; the keymap refuses it.
- **Every glyph drawn as text must be in the bundled fonts.** `crates/throng-app/tests/glyphs.rs`
  fails the build otherwise; write non-ASCII glyphs in source as `\u{…}` escapes.
- **Clipboard chords arrive as events, not keys.** egui-winit turns Ctrl/Cmd+X, C and V into
  `Event::Cut`, `Copy` and `Paste`; matching them as keys never fires.
- **egui_kittest steps at 1/60 s.** A UI test waits on an observable condition, never a sleep.

## GitHub

CI logs: fetch a job's log through the GitHub API tools; direct log blob downloads are blocked from
the cloud container.
