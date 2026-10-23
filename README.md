# Spacewalk

A Rust terminal disk usage explorer inspired by WinDirStat. Browse a file tree above a proportional treemap, with one selection shared between both panels.

![Spacewalk showing a selected directory and its matching blocks](docs/preview.png)

## Installation

### From a release

Install the latest published release:

```sh
curl -fsSL https://raw.githubusercontent.com/mattslaney/spacewalk/main/install.sh | sh
```

Prebuilt binaries are available for Linux and macOS on x86_64 and ARM64.
The installer verifies the download's SHA-256 checksum and installs to
`~/.local/bin`. No Rust toolchain or root access is required. Run the same
command again to update.

If needed, add the installation directory to your PATH:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

To download the script separately and choose a version or installation directory:

```sh
curl -fsSL https://raw.githubusercontent.com/mattslaney/spacewalk/main/install.sh -o install-spacewalk.sh
sh install-spacewalk.sh --version v0.1.0 --install-dir "$HOME/bin"
```

`SPACEWALK_VERSION` and `SPACEWALK_INSTALL_DIR` also set these options.
Remove `~/.local/bin/spacewalk` to uninstall the default installation.

You can also download archives and checksums from
[GitHub Releases](https://github.com/mattslaney/spacewalk/releases). If no release
is available for your platform, build from source.

### From source

Requires Rust 1.88 or newer:

```sh
cargo install --git https://github.com/mattslaney/spacewalk.git --locked
```

Or install from a local checkout:

```sh
make install                 # Or: cargo install --path . --locked
```

Cargo installs to `~/.cargo/bin` by default. To use a different prefix, run
`make install INSTALL_ARGS='--root /path/to/prefix'`; the executable goes in that
prefix's `bin` directory.

## Usage

Spacewalk needs a terminal with at least 60 columns and 20 rows for its interactive
interface. A Unicode font and true color support give the best results.

Run `spacewalk` to explore the current directory, or pass a path. Hidden files are
included.

```sh
spacewalk ~/Downloads
spacewalk . --apparent-size   # Logical lengths rather than allocated blocks
spacewalk . --summary         # Plain text output, including when piped
spacewalk --help
```

## Controls

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Switch focus between tree and map without changing selection |
| ↑ / ↓, j / k | Move through tree rows, or between map blocks by position |
| ← | Collapse in the tree, or move left in the map |
| → / l | Expand in the tree, or move right in the map |
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
| h | Hide / show the selected file or directory in the treemap |
| H | Show all hidden items again |
| m / M | Mark or unmark the selected item for deletion / clear all marks |
| d / Delete | Ask to permanently delete marked items, or the selected item if none are marked |
| e | Show scan errors |
| ? / F1 | Open help |
| q / Ctrl+C | Quit; a deletion already in progress finishes first |

Click either panel to focus it and select an item. In the tree, click a directory's triangle to expand or collapse it. A map click expands the selected file's ancestors and scrolls it into view in the tree. Double-click a block to zoom into its parent directory; right-click the map to zoom out. The wheel over the tree moves its selection. Arrow keys and PageUp/PageDown scroll dialogs.

Press `c` in either panel to copy the selected item's absolute path, including spaces and Unicode characters, without quotes or a trailing newline. Clipboard copying runs in the background and reports success or unavailability below the selection details. Linux needs `wl-copy` from `wl-clipboard` for Wayland, or `xclip`/`xsel` for X11, with access to the desktop session. macOS uses `pbcopy`; Windows uses PowerShell's `Set-Clipboard`. Clipboard support is optional; navigation continues normally when no clipboard is available.

Completion notices and other temporary action messages clear after three seconds, revealing the selected item's details again.

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

The selected repository's Git detail line starts with its upstream remote URL. It uses the checked-out branch's configured remote; if that is unavailable, it shows `origin` or the first configured remote, labeled as a fallback remote. URLs are looked up in the background, and long lines end with an ellipsis. Embedded HTTP credentials are omitted from display.

A selected directory lights up all its file blocks and gets a white outline around its region. Colors are assigned by extension. Small files may have no visible block at terminal resolution; zoom into their parent directory to see more detail. Zero-byte files and empty directories are accessible in the tree.

Filtering keeps matching names and their ancestors in the tree. The map keeps the full size context. Picking a block outside the filter clears the filter so the selected item can appear in the tree. Search is scoped to the directory you are viewing.

To set aside a known large item such as `target/`, select it and press `h`. The treemap excludes its entire subtree and expands the remaining blocks to fill the panel. Hidden entries stay in the tree, dimmed and marked `[hidden]`; their actual sizes and the scan totals are unchanged. Percentages and usage bars show each item's share of the remaining map, and the map title reports the visible size and hidden-item count.

Press `h` on a hidden item to show it again, or `H` to restore everything. Showing a child also reveals any hidden ancestors, while independently hidden siblings stay hidden. Visibility choices survive rescans during the current session, including new files added within a hidden directory. Hiding items never changes files on disk.

## Space accounting and deletion

On Unix, sizes default to allocated file blocks, which handles sparse files better than logical lengths. `--apparent-size` uses logical lengths instead. On other platforms, file lengths are used. Directory metadata is excluded from totals. Descendant symbolic links are counted as links and are never followed. Unix hard-linked files count toward bytes only once per scan; additional links still appear in the tree.

Scans run in a worker thread, with progress and cancellation on quit. Unreadable or disappearing entries are reported as errors, and the interface marks the totals as partial. Press `e` for details or `r` to retry. Filesystem changes appear after a rescan; successful or partially failed deletion automatically triggers one.

Press `m` to mark or unmark items in either panel. Marked tree rows have an amber `×`, and the footer shows the marked count. Marks survive sorting, filtering, zooming, and rescanning when the same item still exists. Press `M` to clear every mark. With any marks set, `d` or Delete opens one confirmation listing all targets, including marks outside the current view. Marking a directory and its children deletes the directory once and counts its contents once. Canceling keeps the marks; confirming deletes the batch in the background and refreshes disk usage. The scan root cannot be marked.

Deletion is **permanent**, including recursive directory deletion. The confirmation displays the path and reported size; only `y` confirms, while Enter cancels. The scan root cannot be deleted. Before removal, the application checks that the target and its ancestors still match the scan. Deleting a symlink removes the link rather than its target. Reported usage is not a guarantee of space freed: hard links, shared extents, and filesystem behavior can affect that.

## Development

Clone the repository and run from the checkout:

```sh
git clone https://github.com/mattslaney/spacewalk.git
cd spacewalk
make run ARGS='/path/to/directory'
```

Without Make, use `cargo run --release --locked -- /path/to/directory`.

```sh
make help                    # List common tasks
make check                   # Formatting, Clippy, Rust/installer tests, release build
make fmt                     # Apply Rust formatting
make test                    # Run tests on their own
make build                   # Build target/release/spacewalk
make clean                   # Remove build artifacts
```

`make check` runs the same checks as CI and needs Python 3.8 or newer for the
offline installer tests. `make fmt-check`, `make lint`, and `make installer-check`
are also available individually. Cargo commands use the committed lockfile.

The tests exercise scanning, hard links and symlinks, sparse files, treemap coverage and hit testing, keyboard and mouse synchronization, filtering, sorting, view restoration, clipboard handling, Git status, deletion, terminal sizes, and the summary CLI. Git tests use temporary local repositories and remotes; deletion tests operate only on temporary fixtures. The screenshot above is rendered from synthetic data using Ratatui's test backend.

The code separates filesystem scanning (`scan`), navigation state (`browser`), weighted map layout (`treemap`), Git checks (`git`), clipboard copying (`clipboard`), deletion (`delete`), event handling (`app`), and rendering (`ui`). The interface uses [Ratatui](https://docs.rs/ratatui/0.30.2/ratatui/) and [Crossterm](https://docs.rs/crossterm/0.29.0/crossterm/).

### Publishing a release

The release workflow builds and tests Linux x86_64/ARM64 and macOS Intel/Apple
Silicon binaries, then publishes archives and SHA-256 checksums to GitHub Releases.
Update the version in `Cargo.toml` and run `cargo check` to update `Cargo.lock`.
Commit and push the changes, then push a matching tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

All four builds must succeed before the release is published. Tags containing a
hyphen are marked as prereleases and are not selected by the default installer.
The Release workflow can also be run manually to check builds and download
artifacts without publishing.

## Contributing

Bug reports, feature ideas, and pull requests are welcome. For bug reports,
include your operating system, Spacewalk version, steps to reproduce, and any
relevant output. Include your terminal application when reporting display or
input problems.

Keep pull requests focused, describe the change, and add tests for new behavior
or bug fixes. Run `make check` before submitting. Screenshots help explain changes
to the terminal interface.

## License

Spacewalk is licensed under the [MIT License](LICENSE).
