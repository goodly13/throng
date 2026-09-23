# throng

A project-first terminal and editor workspace, written in Rust. Each project keeps its terminals,
editors and file tree together, and terminals keep running when the window closes.

throng is a single `throng` binary. It draws its own UI with egui and hands its terminals to a
detached daemon (`throng daemon`), which it starts automatically. Linux and macOS are the main
targets. Windows builds, runs, is packaged and passes its unit tests in CI. Its daemon and UI tests
still drive a Unix PTY, so they run on Linux and macOS only.

What the app must do is written down in [`docs/spec.md`](docs/spec.md).

## Build and run

```sh
cargo run --release -p throng-app                       # open throng
cargo run --release -p throng-app -- ~/code/my-project  # open (or create) the project for a folder
```

At runtime Linux needs `libxkbcommon-x11-0`, which most desktops already have. Nothing else is
required: SQLite and the fonts are built in.

Set `THRONG_HOME=<dir>` to keep every file under one directory, which is handy for trying things
without touching your real projects. Debug builds use a separate `throng-dev` instance, so they
never open a release build's data.

| What | Linux | macOS | Windows |
|---|---|---|---|
| settings.json | `~/.config/throng/` | `~/Library/Application Support/throng/` | `%APPDATA%\throng\config\` |
| database, logs | `~/.local/share/throng/` | `~/Library/Application Support/throng/` | `%APPDATA%\throng\data\` |
| daemon endpoint | `$XDG_RUNTIME_DIR/throng/` | `$TMPDIR/throng-<uid>/` | a per-user named pipe |

## Using it

- **Projects** (left sidebar): each project binds a root folder, and no two projects' folders may
  overlap. `+` creates a project. Right-click a project to edit, reorder, reveal or delete it.
- **Explorer**: double-click a file to open it. Right-click to:
  - open a terminal there;
  - create, rename (F2), cut, copy or paste;
  - move to trash, copy a path, or hide the entry in this project.

  Drag an item onto a folder to move it (hold Ctrl, or Option on macOS, to copy). Drag it onto a
  terminal to type its path, or onto an empty panel to open it there. Files dropped in from the
  system open in editors. With the tree focused, Ctrl+Z and Ctrl+Y undo and redo moves, renames and
  deletes, even after a restart. An undo that no longer matches the disk is refused, with the reason.
- **Workspace**: tabs run across the top, and each tab is a dock of panels. Drag a panel's tab to
  split or stack it. Right-click a panel tab to split, rename, restart or kill it.
- **Terminals**: when you close throng with a command running, it asks first. Busy terminals keep
  running and reattach with their output when you reopen. Idle shells close with the app and start
  fresh next time, in the folder they were last working in.
- **Editors**: each file keeps its encoding, BOM and line endings, and Ctrl+S saves. A file that is
  not valid text is refused rather than silently re-encoded. Quitting never asks about unsaved
  edits: they are kept on disk as you type and come back with their undo history on the next
  launch. Auto-save is off by default (Preferences, *Save automatically*).
- **Status strips**: each editor's strip shows the caret, counts, language, wrap and preview. Each
  terminal's shows the shell, working directory and size. Either can be switched off in Preferences.
- **Links**: file paths (with `:line:col`), web addresses and mail links are underlined in editors
  and terminal output. Ctrl+click (Cmd+click on macOS) follows one, and right-click offers more.
- **Markdown previews**: choose Open Preview on a `.md` file's editor tab or in the tree. The preview
  sits beside its editor and follows unsaved edits. It draws the project's images (and `https:`
  ones, which a setting can turn off), and Back and Forward retrace the links you followed.
- **Find in Files** is a panel of its own. Results are grouped by file, and a click opens the file
  at the match. You can also replace across files, after a warning that it cannot be undone.
- **Sub-workspaces**: right-click a terminal, editor or preview tab → *Sync to Sub-workspace*. The
  panel also appears in a window of its own, showing the same shell or the same document: typing in
  either window reaches the one session, and edits land in the one file.
  - Sub-workspaces are listed in the sidebar: click one to open its window again, or right-click to
    rename or destroy it.
  - They come back after a restart, window positions included.
  - Panels made in a sub-workspace belong to it and start in your home folder.
- **Themes**: fifteen are built in (Preferences → Themes). *Duplicate* makes an editable copy with a
  picker for each of its 39 colour tokens. Your themes are JSON files in `<config>/themes`. A theme
  may set only some tokens, and throng re-reads the files when they change.
- **Key bindings**: change any chord below in Preferences → Key Bindings (Add… captures the next
  chord), or in `<config>/keybindings.json`:
  `{ "version": 1, "bindings": { "navigate.quickOpen": ["Ctrl+Shift+T"] } }`.
  - A chord already used by another command is taken only when you confirm.
  - The terminal's own chords (Ctrl+C, D, Z, A, E, W, U, K, R, L, Q) are never given to a command
    that is live in terminals.
- **Icon packs**: put a `pack.json` in a folder under `<config>/icon-packs`, for example
  `{"name": …, "tokens": {"folder": "📁"}}`, then choose the pack under Settings → Icon pack. It
  replaces the glyphs in the tree and toolbars. Anything the pack leaves out, or that the fonts
  cannot draw, keeps throng's own glyph.

`<config>` is the settings folder from the table above (`throng-dev` for a debug build,
`$THRONG_HOME/config` when that is set). Preferences shows it.

The default chords (Cmd instead of Ctrl on macOS):

| Chord | Does |
|---|---|
| Ctrl+Shift+T | Quick Open: pick a project file by part of its path |
| Ctrl+F, F3 / Shift+F3, Esc | Find in the active editor or terminal; next / previous; close |
| Ctrl+H (Cmd+Alt+F on macOS) | Replace in the active editor |
| Ctrl+G | Go to line |
| Alt+Left / Alt+Right | Back / forward in a focused Markdown preview |
| Ctrl+Shift+F / Ctrl+Shift+H | Find / replace in files |
| Ctrl+Shift+D / Ctrl+Shift+E | Split the active panel right / down with a terminal |
| Ctrl+Shift+W | Close the active panel |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab (new tabs: the tab strip's `+`) |
| Ctrl+Shift+N | New project |
| Ctrl+, | Preferences |
| Ctrl+= / Ctrl+- / Ctrl+0 | Zoom the interface in / out / back |
| Ctrl+Alt+N / F11 | Show or hide the file tree / full screen |
| Ctrl+Shift+C / Ctrl+Shift+V | Copy / paste in a terminal (Cmd+C / Cmd+V on macOS) |

Ctrl+C, Ctrl+D, Ctrl+Z and the other shell chords always reach the shell. Ctrl+C copies only when
there is a selection.

## Packages

`packaging/package.sh` builds the installers for the platform it runs on into `dist/`:

- **Linux**: `.deb`, AppImage (with `appimagetool` on `PATH`) and `.tar.gz`.
- **macOS**: a universal `.dmg`, signed and notarised when `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
  `APPLE_TEAM_ID` and `APPLE_APP_PASSWORD` are set.
- **Windows**: a `.zip`.

The Package workflow runs the script on all three platforms. It checks each package by installing,
mounting or unpacking it and running `throng --version` from it.

## Layout

| Crate | Role |
|---|---|
| `throng-core` | OS-agnostic domain: projects, path rules, layout, terminal rules, settings, themes, key bindings, text codec, notices |
| `throng-platform` | OS layer: directories, shell detection, atomic writes, trash, detached spawn, process trees |
| `throng-persistence` | SQLite: projects, layouts (with quarantine), app state, migrations |
| `throng-protocol` | Daemon wire protocol: messages, framing, output offsets |
| `throng-daemon` | The daemon (PTY sessions) and the client the UI uses |
| `throng-editor` | The editor's text model: rope buffer, selections, undo history, find, wrapping, highlighting |
| `throng-app` | The `throng` binary: UI, terminal widget, editor, explorer |

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The UI tests (`crates/throng-app/tests/ui.rs`) drive the real app through its accessibility tree,
against real `throng daemon` processes. The daemon tests run real shells on real PTYs. Neither kind
may leave a process behind. CI (`.github/workflows/ci.yml`) runs all of it on Linux, macOS and
Windows, plus a headless launch on Linux. To render the app with no display:

```sh
cargo run -p throng-app --example seed -- /tmp/demo-home "$PWD"
THRONG_HOME=/tmp/demo-home THRONG_SCREENSHOT=/tmp/shot.png \
  xvfb-run -a env LIBGL_ALWAYS_SOFTWARE=1 cargo run -p throng-app
```

## Licence

throng is licensed under the GNU Affero General Public License v3.0 only; see [`LICENSE`](LICENSE).
It is a modified work, and [`NOTICE`](NOTICE) says what it derives from.
