# throng — requirements

throng is a project-first terminal and editor workspace. A **project** is a named, coloured root
folder. Its **workspace** is a row of tabs, and each tab is a dock of **panels**: terminals, editors,
Markdown previews and Find in Files. Terminals belong to a detached daemon, so they outlive the
window.

This document says what the app must do. Code comments refer to its principles as *Principle I*,
*Principle II* and so on. Each requirement is either met today, or its *Not yet* note says what is
missing. [Open work](#open-work) collects every gap in one list.

## Principles

- **I. Project-first.** Everything lives inside a project: terminals, editors, the tree and search.
  Project roots are exclusive: no two roots are equal, and none contains another.
- **II. A platform-agnostic core.** Domain rules (`throng-core`) never call the operating system.
  Anything that differs by OS sits behind `throng-platform`, which picks rules per platform (path
  identity, file-name validity, process trees) as values, not scattered `cfg!`s. Linux and macOS
  come first, and Windows must always build.
- **III. Terminals are detached, tagged and persistent.** A daemon owns every PTY. Closing the UI
  never kills a busy terminal without asking. No process a terminal started may outlive it. A
  terminal that fails keeps its last screen readable.
- **IV. Terminal keys belong to the terminal.** throng hosts the user's real shells. The chords a
  shell needs (Ctrl+C, D, Z, A, E, W, U, K, R, L, Q) always reach it, and are never bound to a
  command that is live in a terminal.
- **V. Test first, at the cheapest layer that proves it.** Domain rules have unit tests. The daemon
  is tested against real shells on real PTYs, and the UI through its accessibility tree against a
  real daemon. A test run leaves no process behind.
- **VI. Simple, modern, discoverable.** Every panel action is in a menu, and menus show the chord
  actually bound. One condition raises one notice, and that notice carries the actions that resolve
  it.
- **VII. Reviewed change.** Changes land through review, with CI green on every platform.
- **VIII. SOLID, DRY and YAGNI.** One rule has one implementation. Nothing is built before it is
  needed.
- **IX. Explicit composition.** Each process has one composition root (`main`, or the daemon's
  entry point), which builds its services and passes them down. There is no mutable global state.
- **X. Externalised configuration.** Settings are declared once as metadata, and themes, key
  bindings and icon packs are user-editable files. A malformed file is never overwritten.
- **XI. A dockable workspace.** Panes hold tabs, and tabs hold docked panels. Document state (text,
  encoding, line endings, indentation, undo) belongs to the document, never to a view of it.

## User stories

- **Open a folder as a project.** `throng <folder>`, or the New Project dialog, creates the project
  whose root holds that folder, or activates it if it exists. Its layout opens with a terminal.
- **Terminals outlive the UI.** Closing throng with a command running offers exactly three choices.
  *Leave Running* keeps busy terminals and closes idle ones. On reopening, each busy terminal
  reattaches with its output, and the idle ones start fresh.
- **Arrange the workspace.** Tabs, each a dock: split right or down, stack, drag panels between
  leaves, rename, close.
- **Edit files without corrupting them.** Encoding, BOM and line endings (mixed ones included)
  survive a save byte for byte. A file that is not valid text is refused. An external change never
  silently overwrites unsaved edits.
- **Browse and change the project's files.** A lazy tree offers create, rename, move to trash, copy
  path, reveal and hide per project, and opens items in an editor or a terminal.
- **Find things.** Find in the active panel, Quick Open, Go To Line, and Find and Replace in Files.
- **Show a panel in a second window.** A sub-workspace mirrors a project's terminal or document,
  with input and edits shared.
- **Make it one's own.** Themes, key bindings, icon packs and settings, edited in Preferences or in
  files.

## Functional requirements

### Projects and paths

- **FR-001** Projects MUST have a name, a colour and an exclusive root folder (Principle I). Path
  identity MUST follow the platform:
  - Linux: case-sensitive.
  - macOS (default volumes): case- and normalisation-insensitive.
  - Windows: case-insensitive, with `\` as a separator.

  Lower-casing every path on every OS would fuse `~/Proj` and `~/proj` on Linux.
- **FR-002** File-name validation MUST forbid only what the platform forbids: `/` and NUL on Linux
  and macOS, and the reserved characters and names on Windows.

### Terminals and the daemon

- **FR-003** A detached daemon MUST own every PTY, reached over a per-user, per-instance endpoint:
  a Unix socket in a `0700` directory, or a named pipe on Windows.
  - A second daemon for the same instance MUST refuse to start.
  - A daemon whose endpoint has disappeared MUST end its terminals and exit, rather than linger
    unreachable.
- **FR-004** Each client connection MUST carry one ordered stream, so writes, resizes and detaches
  never overtake each other.
  - Frames MUST be length-prefixed, with a size cap.
  - A malformed frame MUST close only its own connection.
  - The sender MUST chunk input larger than a frame.
  - When a connection drops, every pending request MUST fail at once.
- **FR-005** Reattaching MUST neither duplicate nor lose output:
  - Output carries its byte offset.
  - The attach snapshot is taken under the same lock that orders output.
  - A view drops whatever the snapshot already covered.

  Replayed terminal queries MUST NOT be answered. A program on the alternate screen MUST be nudged
  to repaint.
- **FR-006** A terminal that exits while no view is attached MUST keep its exit status and output
  until an attach reads them.
  - An exit with code 0, or one the user caused, MUST return the panel to the type picker, with the
    last configuration remembered.
  - Any other exit MUST keep the last screen, show the code, and offer Restart and Close.
- **FR-007** A terminal is busy while a process other than its shell holds its foreground process
  group. On Windows, it is busy while the shell has a child process other than a console host. When
  this cannot be determined, the terminal MUST be reported busy.
- **FR-008** Killing a terminal MUST end its process groups, escalating from hang-up to kill. On
  Linux, it MUST also end every process in the terminal's session. On Windows, each shell MUST be
  adopted into a job object that ends its processes when the job closes, so a daemon that goes
  takes its terminals' commands with it. No process a terminal started may outlive it.
- **FR-009** A startup command MUST run once, after the shell's first output, on a cold start
  only, never on reattach. The shell's environment MUST come from the UI at spawn time, with
  `THRONG_*` removed.
- **FR-010** The PTY size MUST be the smallest across attached views. It MUST change only on a real
  change, and ignore resizes from views that are no longer attached.
- **FR-011** Closing with busy terminals MUST offer exactly *Leave Running*, *Terminate All* and
  *Cancel*. Closing MUST NOT ask about unsaved editors, because FR-027 brings them back.
- **FR-012** Terminal keys belong to the terminal (Principle IV):
  - Ctrl+C/D/Z/A/E/W/U/K/R/L/Q reach the shell.
  - Plain Ctrl+C copies only when there is a selection, and app chords use Ctrl+Shift.
  - Bracketed paste MUST neutralise escape sequences, so a paste cannot end the bracket early.
  - Focus reports go out only on a real focus change.
- **FR-013** Terminal fidelity. The emulator runs the kitty keyboard protocol:
  - A program that pushes disambiguation gets these as `CSI code;mods u`: Escape, and Enter, Tab,
    Backspace, Space, letters and digits with Ctrl, Alt or Shift. Shift+Enter differs from Enter.
  - A program that asks for every key gets plain keys that way too.
  - Arrows and function keys keep their CSI forms.

  Colour queries (OSC 10, 11, 12 and 4) are answered with the colour the program set, else
  throng's palette, and so is the text-area size query in pixels. An input method's candidate
  window sits at the cursor, and its composition is drawn there, underlined. While it composes,
  Enter and Backspace belong to the input method.
- **FR-014** A terminal's working directory. throng reads each running shell's directory from
  outside it, every 1.5 s:
  - Linux: `/proc/<pid>/cwd`.
  - macOS: `proc_pidinfo`.

  This way no hook is installed in anyone's shell, and every shell is covered. An OSC 7 report
  from this machine takes precedence: a nested shell or `tmux` hides the outer process's directory.

  A panel remembers where it last worked. This is on unless the panel or
  `terminal.defaultRememberDirectory` says otherwise. A cold start returns there, or to the folder
  it was asked to start in, but only while that is a folder inside the project; otherwise it
  silently uses the project root.

  The type picker takes shell arguments (quoted as a shell would read them) and the per-panel
  choice "Reopen in the last directory", and remembers both.
  *Not yet:* Windows has no live working directory beyond OSC 7.
- **FR-015** A terminal's text (Copy All, and what the tests read) joins the rows the emulator
  wrapped, and counts a wide character once.
- **FR-016** Quitting leaves terminals as they should be. Once quitting is confirmed, a terminal's
  end changes no panel, and nothing attaches or starts a terminal.
- **FR-046** Command memory. A terminal panel can remember the command running in it. The
  picker's *Remember the running command* checkbox is off by default, and it never touches
  directory memory.
  - **Observing.** While it remembers, throng notes what the shell runs, on the same 1.5 s
    observation as the working directory, using one process snapshot for every terminal. What runs
    is the shell's most recently started direct child; a copy of the shell itself (a subshell) does
    not count, and neither do grandchildren. It is read from outside the shell (`/proc` on Linux,
    libproc on macOS), so no shell needs a hook.
  - **Keeping.** The last command seen is saved with the layout, so an end nobody saw (a crash, a
    daemon or machine restart) still captures it: the next cold start makes it the startup command
    before running it.
  - **Capturing.** Killing the terminal, and *Terminate All* when quitting, read what runs at that
    moment. A command becomes the startup command, and the picker shows it; nothing running leaves
    the startup command as it was. A shell that exits on its own captures nothing. *Leave Running*
    is not an end, so a busy terminal left running captures nothing yet.
  - **Limits.** A captured command is one line of at most 2,048 characters with no control
    characters. Its words are quoted so the shell reads them back unchanged, and it runs on the
    next cold start exactly as a typed startup command would.

  *Not yet:* Windows (its shells report no running command).
- **FR-047** Manual reload. `terminal.reloadMode` (Preferences, *Start terminals*) is *automatic* by
  default.
  - **Dormant panels.** In *manual* mode, the terminals a saved layout holds when it loads do not
    start. A terminal still running reattaches as usual. Any other shows a placeholder naming the
    panel, with a **Reload** button, and holds no shell.
  - **Reloading.** The panel's tab menu offers *Reload Terminal* in place of *Restart Terminal*.
    Reloading is an ordinary cold start (FR-046 captures apply).
  - **What stays automatic.** Terminals made during the session (the picker, a split) start at once.
    A dormant panel keeps its name, type and place, and stays dormant when its project is switched
    away and back.
  - **Changing the mode.** A change applies to layouts loaded after it.

### Workspace and layout

- **FR-017** Layouts MUST use throng's own versioned model, converted to and from the docking
  widget at the UI edge.
  - A layout that cannot be read MUST be quarantined before a fresh one replaces it, and the user
    told once.
  - A persistence write MUST touch only the row it means.
- **FR-018** Migrations MUST be idempotent, and each MUST be stamped in the same transaction as its
  step. A database written by a newer build MUST be refused, not downgraded.
- **FR-019** Every panel tab MUST carry an accessible name.
- **FR-020** Status strips:
  - **Editors** get a one-line strip. On the left are readouts: `Ln`, `Col`, the selected
    characters when there is a selection, and the document's characters and words, recounted at
    most every 150 ms. Characters count UTF-16 units with line breaks; a word is a run of
    non-whitespace. Each readout is named in full for assistive technology.
  - The editor strip's right side holds actions: the language picker, the format, the word-wrap
    toggle, and Preview for a file that has one (lit while the preview is open).
  - On a narrow panel, labels shorten first. Then readouts are dropped in a fixed order: words,
    characters, selection, column, line. The language and wrap are never dropped. Figures are
    digit-grouped.
  - **Terminals** get a strip naming the shell, where it is working, and its grid.
  - Four preferences govern the strips: `editor.showStatusBar`,
    `editor.statusBar.showCursorPosition`, `…showCounts` and `terminal.showStatusBar`.
- **FR-021** Sub-workspaces. A sub-workspace is a window of its own that holds tabs of panels and is
  listed in the sidebar.
  - **Mirrors.** Every project terminal, editor and preview offers **Sync to Sub-workspace ▸ New
    Sub-workspace | ‹sub-workspace› ▸ New Tab | ‹tab›**, which shows the panel there as a mirror.
    A mirror is the same terminal session (input from either window reaches the one shell) or the
    same document (one buffer and dirty state, with each view keeping its own caret). It is titled
    with its project.
  - **Size and focus.** A mirrored terminal is sized by its project's panel while that panel is on
    screen, and by the mirror otherwise. Focus in any window showing it counts as focus for the
    program.
  - **Requests.** Whatever a mirror asks of its content (save, reload, restart, go to line) is done
    to its project's panel.
  - **Own panels.** Panels made in a sub-workspace belong to it, and their terminals start in the
    home folder. A project's file is never opened into, dropped on, or saved from one of these
    editors.
  - **Closing.** **Close Here** closes a mirror only in that window. Destroying the project's panel
    removes its mirrors everywhere. A sub-workspace left with no tab goes. **Close Window** hides a
    sub-workspace, and the sidebar opens it again. **Destroy** ends it, with its own panels, and
    closes the mirrors.
  - **Persistence.** Sub-workspaces, their tabs and where each window sits are saved and restored.
    A window saved off every screen opens on one.

  *Not yet:* a single focus group (raising one window raises them all), tearing a tab out by
  dragging it, and moving a panel rather than showing it.

### Editors and documents

- **FR-022** Documents MUST round-trip byte for byte: UTF-8 with or without a BOM, UTF-16 LE/BE, and
  LF, CRLF, CR or mixed line endings. Only edited lines take the dominant ending. Invalid UTF-8 and
  binary files MUST be refused rather than decoded lossily.
- **FR-023** Saves MUST be atomic: write a temp file, fsync, rename, keep permissions and follow
  symlinks. The saved snapshot MUST be exactly the text written. Line endings MUST belong to the
  document, never to a view.
- **FR-024** Panels share one buffer per file, keyed by the resolved path. A clean buffer follows
  the disk; a dirty one is marked and never overwritten. The echo of our own save is not a change. A
  deleted file keeps its buffer, dirty.
- **FR-025** Find in the active panel. One find bar serves editors and terminals:
  - It matches literal text, optionally case-sensitive and whole-word.
  - It is incremental, shows a count, and highlights every match.
  - F3 and Shift+F3 step through matches, and Esc closes the bar.
  - In an editor, Replace All is one undo step.
  - In a terminal, find searches the scrollback and never writes to the program.
- **FR-026** Go To Line and Quick Open. In Quick Open:
  - Every term matches part of the path, in any order.
  - A file-name hit ranks above a folder hit, and ties keep a stable order.
  - The list is built in the background and capped.
  - Files hidden in the project appear only on request.
- **FR-027** Crash recovery. Every document with unsaved changes has a recovery file:
  - It is written atomically 400 ms after the last edit, off the UI thread.
  - It lives in a private folder (`0700`, with files `0600`).
  - It holds the text and, unless `editor.persistUndoHistory` is off, up to 1 MiB of undo history.
  - It is removed when the document is saved, becomes clean or is closed.

  At launch, each document gets back its text, history, language and wrap when its editor opens.
  Records that no saved layout shows are deleted. Turning history persistence off purges history
  from every record at once. A file deleted while throng was closed comes back as an unsaved editor
  for that path.
- **FR-028** Auto-save is off by default. When it is on, a document with a path is saved
  `editor.autoSaveDebounceMs` (300 ms) after its last edit. An untitled document is never
  auto-saved, and neither is one whose file changed or was deleted elsewhere; the user decides.
- **FR-029** Highlighting is incremental. State is checkpointed every 32 lines, and each frame has a
  time budget, so a large file never stalls a frame. Lines over 10,000 characters are drawn plain.
  Language coverage is bat's syntax set.

### Explorer and file operations

- **FR-030** The explorer MUST watch only expanded directories, because of Linux's inotify limit.
  - Copy and move MUST share one rule that refuses a folder into its own subtree.
  - A trash operation MUST report the outcome for each item.
  - A tree row MUST take its own clicks.
- **FR-031** Undo of tree file operations.
  - **What is recorded.** Moves, renames and deletes made from the tree go on a per-project history
    of 50. It is kept in the database and deleted with the project. Copies and new files are not
    recorded.
  - **Keys and menus.** With the tree focused (it was clicked last, and nothing in it is being typed
    into), Ctrl+Z undoes, and Ctrl+Y or Ctrl+Shift+Z redoes. The tree's menus carry Undo and Redo,
    disabled when there is nothing to undo or redo.
  - **Checks.** Every undo and redo is first checked against the disk. It is refused if an item is
    gone, a path is taken, a folder no longer exists, or a trash item was emptied. The error notice
    names the item and the reason, and the history is left as it was.
  - **Editors.** Open editors follow an undone move without becoming dirty.
  - **Deletes.** On Linux and Windows a delete is undone from the trash. macOS's trash cannot be
    restored from programmatically, so deletes there are not recorded; the Finder's *Put Back* still
    works.
- **FR-032** Drag and drop.
  - **Within the tree.** Dropping onto a folder moves into it; dropping onto a file moves into that
    file's folder.
  - **Copying.** Ctrl (Option on macOS) copies instead. A copy that would collide is named
    `name copy`, then `name copy 2`.
  - **No-ops and refusals.** A move into the folder the item is already in does nothing. A move is
    refused, with the reason, when the name is already taken, the target is the folder itself or
    the root, or the destination is outside the project.
  - **Keyboard and menus.** Cut, Copy and Paste do the same. A moved folder keeps its expansion.
  - **Onto a terminal.** The path is pasted quoted, with a trailing space and no newline, so nothing
    runs.
  - **Onto an empty panel.** A file opens there, or the editor that already has it is focused. A
    folder is refused.
  - **From the system.** Files dropped in open in editors of the active project. Folders, and files
    outside the project, are refused, and the notice says why.
- **FR-033** The tree's Cut, Copy and Paste answer the platform's clipboard events, which is how
  Ctrl/Cmd+X, C and V arrive. Cut and Copy also put the item's path on the system clipboard.
- **FR-034** Changes to a folder are reported under the path the folder was watched by. For
  example, macOS reports `/var/…` as `/private/var/…`.

### Search

- **FR-035** Find in Files is a panel.
  - **Searching.** It waits for typing to settle and streams results grouped by file. It counts
    what it skipped (binary or too large), and it never follows a symlinked folder.
  - **Replacing.** Replace in files re-checks every match against the file as it is now, and
    refuses the matches that changed. It keeps each file's encoding and line endings. It warns
    first that it cannot be undone (setting `search.warnIrreversibleCommit`).
  - **Scope.** A scope outside the project is refused.
- **FR-036** Paths a scan reports are spelled under the project root as the user gave it, never in
  its canonical form. That way results, open editors and the explorer agree on one path.

### Links and previews

- **FR-037** Links.
  - **Grammar.** Editors and terminals share one grammar (`throng_core::links`), which recognises:
    - web addresses, `mailto:` and `tel:`, and bare email addresses;
    - `file:` URIs (percent-decoded, and refused if they decode to a control character);
    - paths: absolute, `~/`, `./`, `../`, drive and UNC forms, a relative path whose last part has
      an extension, or any name with a position (`:line`, `:line:col` or `(line,col)`).

    Trailing punctuation and unbalanced brackets are trimmed, and a quoted path may hold spaces.
    `javascript:`, `data:` and every other scheme are never links.
  - **Detection.** Detection is syntactic and covers only what is on screen. A path that wraps
    across terminal rows is one link. OSC 8 hyperlinks are judged on their target.
  - **Display.** Links are underlined faintly, solid under the pointer, with a tooltip.
  - **Following.** Ctrl+click (Cmd+click) follows a link, as does Ctrl+Enter at the caret in an
    editor. A Ctrl+click is never sent to a terminal program as a mouse report.
  - **Relative paths.** A relative path is tried in two places, and the first that exists wins:
    first beside the editor's file (for a terminal, where the shell is working now, else where it
    started), then at the project root.
  - **Targets.** Following opens a project file at its position. It reveals a folder, or anything
    outside the project, in the file manager, and it reports a missing file. It never runs a
    program: a program is revealed instead.
  - **Menu.** Right-clicking a link offers Open Link and Copy Link Address, plus Reveal in File
    Manager and Open with Default Program for files.
  - **Settings.** `editor.links.detectInEditors` and `editor.links.detectInTerminals` switch
    detection off.
  - **Drive forms.** On Windows, the drive paths that Git Bash, MSYS, Cygwin and WSL print
    (`/c/…`, `/cygdrive/c/…`, `/mnt/c/…`) name the Windows drive (`C:\…`). They are tried before
    the project root, and an existing project file still wins over a drive path that does not
    exist.
- **FR-038** Markdown previews. `Preview` is a panel kind, saved with the layout and bound to a file.
  - **Opening.** A preview opens from an editor's tab and content menus and from the tree's menu.
    It goes beside the file's editor, on the right, when the file is open in one; otherwise it
    stands alone. A second request focuses the existing preview.
  - **Following.** It follows the open document's unsaved text 300 ms after the last edit, and at
    most 1 s behind while typing (`editor.previews.updateDelayMs` and `…maxWaitMs`). When no editor
    has the file, it follows the file on disk, read without opening a document. It keeps its scroll
    position.
  - **Rendering.** It renders CommonMark and GitHub's extensions: headings, emphasis,
    strikethrough, lists, read-only task boxes, quotes, tables, rules, links, and bare-address
    autolinks. Code blocks are highlighted with the editor's syntax colours. YAML front matter shows
    as a key/value table, and invalid front matter as code.
  - **Raw HTML.** Raw HTML is sanitised: `kbd`, `sub`, `sup`, `br`, `summary` and `img` render,
    every other tag is dropped, and a script's or style's content never reaches the page.
  - **Links.** Ctrl+click follows a link:
    - `#heading` scrolls to the heading.
    - Another Markdown file in the project opens in the same preview, at its heading.
    - Other project files open in an editor.
    - Web and mail links go to the system.
    - Anything missing or outside the project gives one inline notice.
  - **Tab.** A preview is titled `<name> - Preview` and is dirty exactly while its source is. It
    cannot be renamed, and it closes without a prompt.

  *Not yet:* images (their alternative text shows instead), scroll sync with the editor, and
  back/forward history.

### Appearance and configuration

- **FR-039** Settings MUST be declared once, as metadata.
  - A missing file is seeded.
  - A malformed file is never written. Defaults run in memory, and one notice says so.
  - Out-of-bounds values are clamped and written back once.
  - Unknown keys MUST survive a write, but retired keys are dropped.
- **FR-040** Notices MUST be keyed by condition. Raising a condition that is already showing updates
  that notice and never adds a second (Principle VI). Startup failures MUST be shown and logged,
  never silent. Logs rotate at a size cap.
- **FR-041** Themes.
  - **Built-in themes.** Fifteen ship, with 39 colour tokens across General, Editor, Syntax,
    Terminal and Search. A derived theme lifts every syntax hue to 6:1 on the editor body. It tints
    the search surfaces only as far as code still reads at 4.5:1 through them. The gutter is the
    body offset by 9% on a dark theme and 6% on a light one.
  - **Choosing.** `appearance.theme` names a theme; the default is `throng`. `dark`, `light` and
    `system` also resolve. A name no theme has falls back to `throng`, and the file is not
    rewritten.
  - **User themes.** These are JSON files in `<config>/themes`, and the folder is watched. A file may
    name only some tokens; the rest come from `throng`. A file that cannot be used is left alone and
    listed in one notice: it is unreadable, has no name, uses a built-in's name, or uses a name
    another file already has.
  - **Editing.** Preferences → Themes picks a theme, duplicates one into an editable copy, and
    deletes a copy (to the trash). Each token is edited with a colour picker, and a text token shows
    its contrast ratio when it falls under 4.5:1. Edits are drawn at once, and written once they
    settle, keeping whatever else the file holds.
  - **Project colour.** The project colour remains the project's mark. The caret, the selection and
    every surface belong to the theme.
- **FR-042** Key bindings.
  - **The keymap.** One keymap holds every chord. It has 36 commands, each with the scopes it is live
    in (editors, terminals, the file tree, previews, Find in Files) and its defaults. The macOS
    defaults differ only where they must: Cmd+H hides an app, so Replace is Cmd+Alt+F there.
  - **The file.** `<config>/keybindings.json` is
    `{ "version": 1, "bindings": { "<command>": ["<chord>", …] } }`, and `Ctrl` in a chord means Cmd
    on macOS. The file is watched.
    - A command the file lists takes exactly the file's chords, so an empty list unbinds it.
    - Commands at their defaults are never written.
    - What cannot be used is listed in one notice, and the rest applies.
    - Command names that throng does not have yet are passed over without a notice.
  - **Preferences → Key Bindings.** It lists every command with its chords. It can filter, remove a
    chord, capture a new one (Escape cancels) and reset. A chord that another command runs where this
    one is live is taken only after *Reassign*.
  - **Terminal tiers.**
    - The reserved tier (Ctrl+C, D, Z, A, E, W, U, K, R, L, Q) is refused for any command live in a
      terminal, whether the chord comes from the editor or from the file.
    - The shadowable tier (Ctrl+B, F, N, P, H, S) is allowed, and the row names what terminals lose.
  - **Display.** Menus and tooltips show the chord actually bound.
  - **Fixed editor chords.** In editors, undo, redo, select all and the clipboard keep the platform's
    chords.
- **FR-043** Icon packs.
  - **Tokens.** The glyphs the interface draws as text are tokens: tree folders, files and chevrons,
    toolbar and find-bar buttons, and dismiss, add and reset.
  - **Packs.** A pack is `<config>/icon-packs/<name>/pack.json`, of the form
    `{ "name": …, "tokens": { "<token>": "<glyph>" | { "glyph": "<glyph>" } } }`.
    `appearance.iconPack` names the pack in use.
  - **Fallback.** A token keeps throng's own glyph if the pack leaves it out, sets it to an image, or
    sets it to a glyph the fonts cannot draw. One notice says which tokens fell back.

  *Not yet:* image (SVG) icons.
- **FR-044** Every glyph drawn as text MUST exist in the bundled fonts, and a test enforces it.

### Packaging

- **FR-045** Packages. `packaging/package.sh` builds packages for the platform it runs on:
  - **Linux:** a `.deb`, an AppImage and a `.tar.gz`. The `.deb` declares the X11, Wayland and
    OpenGL libraries loaded at run time, and the oldest glibc the binary needs.
  - **macOS:** a universal `throng.app` (Apple silicon and Intel) in a `.dmg`. It is signed with a
    Developer ID and notarised when those credentials are given, and ad-hoc signed otherwise.
  - **Windows:** a `.zip`. A release build opens no console window.

  The Package workflow builds each package, then installs, mounts or unpacks it, and runs it.
  Inside an AppImage the daemon is started from the AppImage itself, so it keeps a mount of its own
  after the UI's mount closes. The window carries throng's icon.
  *Not yet:* an `.msi`, and publishing packages to a GitHub release for a version tag.

## Open work

In rough order of value:

1. **Windows as a first-class target.**
   - Run the daemon and UI tests on Windows (they drive a Unix PTY today).
   - A live working directory and command memory for Windows shells (FR-014, FR-046).
   - Elevated and de-elevated terminals, and WSL shells.
2. **Publishing.** An `.msi`, and attaching every package to a GitHub release for a `v*` tag
   (FR-045).
3. **Sub-workspaces** (FR-021): the single focus group, dragging a tab out to tear it off, and moving
   a panel rather than showing it.
4. **Icon packs:** image (SVG) icons (FR-043).
5. **Markdown previews:** images, scroll sync, and back/forward history (FR-038).

## Done means

- **SC-001** `.github/workflows/ci.yml` passes on Linux, macOS and Windows: fmt, clippy with warnings
  denied, and every test.
- **SC-002** The reattach UI test passes: a busy terminal survives closing throng and reattaches with
  its output exactly once, while the idle one is closed and restarted fresh.
- **SC-003** A test run leaves no daemon, shell or child process behind.
- **SC-004** The headless Linux launch of the real binary (seeded project, Xvfb) produces a
  screenshot.
- **SC-005** The Package workflow builds every package and runs each one from where a user would.
