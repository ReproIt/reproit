# tauri-linux: why the second application is still missing

The tauri-linux probe observes one channel: the WebKitGTK webview's DOM, over
WebDriver through tauri-driver. Everything a campaign does, setup included, has
to be reachable from there. This records what was built, what was measured, and
which candidate applications that channel rules out.

## readest was built, not campaigned

The build recipe is proven and committed as
`validation/field/tauri-linux/stage-readest.sh`. It produced a Linux x86_64
binary at the fixed revision `09548d998f16be10315d988176a2e7acddce3473`
(732 MB, `/work/target/debug/readest`). Four traps were found and are fixed in
that script, each of which fails in a way that looks like a different problem:

1. `packages/foliate-js` is a submodule. Without
   `git submodule update --init --recursive` the vendor step dies inside
   `npx postcss` with "You must pass a valid list of files to parse", which
   reads as a tooling fault.
2. `roaring 0.11.4` refuses to build on Rust 1.88, so the worker image moved to
   Rust 1.90. The failure is a dependency-resolution error, not a compile error.
3. One rustc per core exhausts the container's memory on this dependency graph.
   An out-of-memory rustc leaves half-written artifacts and cargo then reports
   `can't find crate for cc` inside build scripts. `CARGO_BUILD_JOBS=4` fixes it.
4. readest is a cargo workspace, so the binary lands in the workspace target
   directory at the repository root, not under `src-tauri`.

## The blocking measurement: the library cannot be seeded through the webview

The selected defect, issue 5175 (the select-mode action bar hides the last book
in list view), needs a library with enough books that the last row falls under
the bar. readest imports books through a native GTK file chooser, which the
webview channel cannot see.

readest also accepts file paths on argv (`get_files_from_argv` in
`apps/readest-app/src-tauri/src/lib.rs`), so the campaign tried that. Measured,
on the fixed build, with 24 generated plain-text books:

- Launching with the 24 paths opens them: the webview lands on
  `tauri://localhost/reader?ids=...` with 24 grid cells, one foliate view each.
- Navigating to `tauri://localhost/library` in the same process shows
  "Start your library", the empty state.
- Quitting and launching a second time in the same container, with no
  arguments, also shows "Start your library" with no bookshelf element.

So argv opens books transiently and does not import them. Every remaining path
into the library is a native window.

## What that rules out

- `note-gen-export-filename-516`: the observable is a GTK file chooser's
  filename entry. Disqualified, not deferred: the probe cannot see a native
  window at all.
- `readest-select-mode-last-book-5200` and the other readest selections: setup
  requires the same native chooser. Disqualified for this harness as it stands.

## The exact missing input

One of:

1. A native-window channel for the Tauri worker. The worker already installs
   `at-spi2-core`, and the repository already drives AT-SPI for linux-gtk, so a
   second observation channel that can drive a GTK file chooser is the smallest
   change that unblocks both readest and note-gen. It is a harness capability,
   not a campaign, and it must be proven on a fixture before any campaign uses
   it.
2. A second independent Tauri application whose defect and whose setup both live
   inside the webview, the way cc-switch's does. cc-switch qualified because its
   entire trigger is a pointer press on a bundled preset list with no imported
   user data.

Until one of those exists, `tauri-linux` has one of the two independent
application campaigns the promotion contract requires, and stays Preview.
