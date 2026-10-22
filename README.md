# Spacewalk

A Rust terminal disk usage explorer inspired by WinDirStat. Browse a file tree above a proportional treemap, with one selection shared between both panels.

![Spacewalk showing a selected directory and its matching blocks](docs/preview.png)

## Run

Requires Rust 1.88 or newer and a terminal with at least 60 columns and 20 rows. A Unicode font and true color support give the best results.

```sh
cargo install --path . --locked
spacewalk ~/Downloads
```

Or run directly from the checkout:

```sh
cargo run --release -- /path/to/directory
```

The directory defaults to the current directory. Hidden files are included.

```sh
spacewalk . --apparent-size   # Logical lengths rather than allocated blocks
spacewalk . --summary         # Plain text output, including when piped
spacewalk --help
```

## Controls

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Switch focus between tree and map without changing selection |
| ↑ / ↓, j / k | Move through tree rows, or between map blocks by position |
| ← / →, h / l | Collapse / expand in the tree, or move left / right in the map |
| Space | Toggle the selected directory in the tree |
| Enter | Zoom into the selected directory; on a map file, zoom into its parent |
| Backspace | Zoom out one directory |
| Home / End, g / G | First / last visible tree row |
| PageUp / PageDown | Move a page through the tree |
| / | Start live, case-insensitive name filtering |
| Enter while searching | Keep the filter and return to navigation |
| Esc | Clear the filter, cancel deletion, or close a dialog |
| n / N | Next / previous matching name in the current scope |
| s | Cycle sort: largest, name, file count, smallest |
| c | Copy the selected item's full path to the system clipboard |
| r | Rescan the original directory and preserve the current view where possible |
| v | Hide / show the selected file or directory in the treemap |
| V | Show all hidden items again |
| d / Delete | Ask to permanently delete the selected item |
| e | Show scan errors |
| ? / F1 | Open help |
| q / Ctrl+C | Quit; a deletion already in progress finishes first |

Click either panel to focus it and select an item. In the tree, click a directory's triangle to expand or collapse it. A map click expands the selected file's ancestors and scrolls it into view in the tree. Double-click a block to zoom into its parent directory; right-click the map to zoom out. The wheel over the tree moves its selection. Arrow keys and PageUp/PageDown scroll dialogs.

Press `c` in either panel to copy the selected item's absolute path, including spaces and Unicode characters, without quotes or a trailing newline. Clipboard copying runs in the background and reports success or unavailability below the selection details. Linux needs `wl-copy` from `wl-clipboard` for Wayland, or `xclip`/`xsel` for X11, with access to the desktop session. macOS uses `pbcopy`; Windows uses PowerShell's `Set-Clipboard`. Clipboard support is optional; navigation continues normally when no clipboard is available.

Repository roots have a `⎇` icon in the tree, including linked worktrees, submodules, and bare repositories. Status icons appear alongside it:

| Icon | Meaning |
| --- | --- |
| `…` | Background check in progress |
| `✓` | Clean working tree; every local branch matches its remote upstream |
| `*` | Staged, unstaged, or untracked changes |
| `↑` / `↓` | At least one local branch is ahead of / behind its remote upstream |
| `∅` | No remote is configured |
| `?` | Status unverified: unavailable Git, unfetched remote, missing upstream, or a branch without a remote upstream |
| `◇` | Detached HEAD; sync is unverified |
| `≡` | Stashed changes exist |

Green means synced, purple identifies repositories without remotes, amber marks other local or uncertain state, and gray means checking. Select the repository for a text explanation below the panels; `?` opens the icon legend. Checks run once in the background after each scan, use all local branches, and leave navigation responsive. Remote comparisons use **last-fetched local refs**; Spacewalk does not fetch or contact remotes. Fetch with Git separately, then press `r` to rescan and recheck. A clean checked-out branch alone is not enough for `✓`: other local branches, missing upstreams, detached HEAD, and stashes also prevent it.

A selected directory lights up all its file blocks and gets a white outline around its region. Colors are assigned by extension. Small files may have no visible block at terminal resolution; zoom into their parent directory to see more detail. Zero-byte files and empty directories are accessible in the tree.

Filtering keeps matching names and their ancestors in the tree. The map keeps the full size context. Picking a block outside the filter clears the filter so the selected item can appear in the tree. Search is scoped to the directory you are viewing.

To set aside a known large item such as `target/`, select it and press `v`. The treemap excludes its entire subtree and expands the remaining blocks to fill the panel. Hidden entries stay in the tree, dimmed and marked `[hidden]`; their actual sizes and the scan totals are unchanged. Percentages and usage bars show each item's share of the remaining map, and the map title reports the visible size and hidden-item count.

Press `v` on a hidden item to show it again, or `V` to restore everything. Showing a child also reveals any hidden ancestors, while independently hidden siblings stay hidden. Visibility choices survive rescans during the current session, including new files added within a hidden directory. Hiding items never changes files on disk.

## Space accounting and deletion

On Unix, sizes default to allocated file blocks, which handles sparse files better than logical lengths. `--apparent-size` uses logical lengths instead. On other platforms, file lengths are used. Directory metadata is excluded from totals. Descendant symbolic links are counted as links and are never followed. Unix hard-linked files count toward bytes only once per scan; additional links still appear in the tree.

Scans run in a worker thread, with progress and cancellation on quit. Unreadable or disappearing entries are reported as errors, and the interface marks the totals as partial. Press `e` for details or `r` to retry. Filesystem changes appear after a rescan; successful or partially failed deletion automatically triggers one.

Deletion is **permanent**, including recursive directory deletion. The confirmation displays the path and reported size; only `y` confirms, while Enter cancels. The scan root cannot be deleted. Before removal, the application checks that the target and its ancestors still match the scan. Deleting a symlink removes the link rather than its target. Reported usage is not a guarantee of space freed: hard links, shared extents, and filesystem behavior can affect that.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

The tests exercise scanning, hard links and symlinks, sparse files, treemap coverage and hit testing, keyboard and mouse synchronization, filtering, sorting, view restoration, clipboard handling, Git status, deletion, terminal sizes, and the summary CLI. Git tests use temporary local repositories and remotes; deletion tests operate only on temporary fixtures. The screenshot above is rendered from synthetic data using Ratatui's test backend.

The code separates filesystem scanning (`scan`), navigation state (`browser`), weighted map layout (`treemap`), Git checks (`git`), clipboard copying (`clipboard`), deletion (`delete`), event handling (`app`), and rendering (`ui`). The interface uses [Ratatui](https://docs.rs/ratatui/0.30.2/ratatui/) and [Crossterm](https://docs.rs/crossterm/0.29.0/crossterm/).

Licensed under MIT.
