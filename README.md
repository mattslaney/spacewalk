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
| + / - | Expand / collapse all folders beneath the selected folder (tree focus) |
| Enter | Zoom into the selected directory; on a map file, zoom into its parent |
| Backspace | Zoom out one directory |
| Home / End, g / G | First / last visible tree row |
| PageUp / PageDown | Move a page through the tree |
| / | Start live, case-insensitive name filtering |
| Enter while searching | Keep the filter and return to navigation |
| Esc | Clear the filter, cancel deletion, or close a dialog |
| n / N | Next / previous matching name in the current scope |
| s | Cycle sort: largest, name, file count, smallest |
| F2 | Set minimum size and minimum modification age for both panels |
| y | Copy the selected item's full path to the system clipboard |
| c / x / p | Queue items for copy / cut / paste |
| a / R | Create a folder / rename the selected item |
| F3 / F4 | Show or hide operation progress / request cancellation |
| Ctrl+g | Open Git reports, sync, cleanup and worktree management |
| t | Open a shell in the selected directory, or the parent of a selected file |
| r | Rescan the original directory and preserve the current view where possible |
| h | Hide / show the selected file or directory in the treemap |
| H | Show all hidden items again |
| m / M | Mark or unmark the selected item for batch actions / clear all marks |
| d / Delete | Ask to permanently delete marked items, or the selected item if none are marked |
| e | Show scan errors |
| ? / F1 | Open help |
| q / Ctrl+C | Quit; a deletion already in progress finishes first |

With tree focus, press `+` to expand the selected folder and all its descendant
folders, or `-` to collapse the whole selected subtree. Selection stays on that
folder, and sibling subtrees keep their current expansion state. These controls
also work during name search and age/size filtering, and their expansion choices
survive rescans. Files and symlinks have no subtree to expand. Map focus keeps its
existing navigation; switch to the tree with Tab before using these shortcuts.

Click either panel to focus it and select an item. In the tree, click a directory's triangle to expand or collapse it. A map click expands the selected file's ancestors and scrolls it into view in the tree. Double-click a block to zoom into its parent directory; right-click the map to zoom out. The wheel over the tree moves its selection. Arrow keys and PageUp/PageDown scroll dialogs.

Press `y` in either panel to copy the selected item's absolute path, including spaces and Unicode characters, without quotes or a trailing newline. Clipboard copying runs in the background and reports success or unavailability below the selection details. Linux needs `wl-copy` from `wl-clipboard` for Wayland, or `xclip`/`xsel` for X11, with access to the desktop session. macOS uses `pbcopy`; Windows uses PowerShell's `Set-Clipboard`. Clipboard support is optional; navigation continues normally when no clipboard is available.

Use `c` or `x` to queue the selected item, or all marked items, for copying or
moving. Navigate to a destination and press `p`. The paste dialog shows the queued
items and an editable destination directory; Tab completes an unambiguous directory
name. Enter confirms and Esc cancels the dialog. Queued transfers are separate from
the system clipboard. Copy queues can be pasted repeatedly; successfully moved
items leave the cut queue. Esc outside a dialog clears the transfer queue.

Press `a` to create a folder in the selected directory (or the parent of a selected
file), or `R` to rename the selected item. Existing destinations are never
overwritten or merged. Directory copies preserve symbolic links as links, file
permissions, file modification times, and sparse zero-filled regions. Ownership, ACLs,
extended attributes, and hard-link relationships are not copied. Moves work across
filesystems; cross-filesystem moves finish the copy before removing the source.
The scan root cannot be moved or renamed.

File operations open a floating progress panel showing the action, destination,
elapsed time, and recent activity, including byte counts during large file copies.
Esc hides the panel so you can keep browsing; F3 reopens it. F4
requests cancellation; completed items are kept and unfinished cut items stay
queued. The result dialog reports failures and partial completion, and disk usage
refreshes automatically. Other mutations and shell launches wait until the operation
finishes. Quitting requests cancellation and waits for the worker to stop safely.

Completion notices and other temporary action messages clear after three seconds, revealing the selected item's details again.

Press `t` in either panel to open an interactive shell in the current terminal. A selected file or symbolic link opens its parent directory. Unix uses `$SHELL` (falling back to `/bin/sh`); Windows uses `%COMSPEC%` (falling back to `cmd.exe`). Type `exit` to return to the same selection, expanded tree, and deletion marks. Press `r` afterward to refresh disk usage and Git indicators if commands changed anything. Finish the current scan or deletion before opening a shell.

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

Green means synced, purple identifies repositories without remotes, amber marks other local or uncertain state, and gray means checking. Select the repository for a text explanation below the panels; `?` opens the icon legend. Checks run once in the background after each scan, use all local branches, and leave navigation responsive. Remote comparisons use **last-fetched local refs**; automatic status checks do not fetch or contact remotes. Use the Git action panel or fetch in the shell, then press `r` to rescan and recheck. A clean checked-out branch alone is not enough for `✓`: other local branches, missing upstreams, detached HEAD, and stashes also prevent it.

The selected repository's Git detail line starts with its upstream remote URL. It uses the checked-out branch's configured remote; if that is unavailable, it shows `origin` or the first configured remote, labeled as a fallback remote. URLs are looked up in the background, and long lines end with an ellipsis. Embedded HTTP credentials are omitted from display.

A selected directory lights up all its file blocks and gets a white outline around its region. Colors are assigned by extension. Small files may have no visible block at terminal resolution; zoom into their parent directory to see more detail. Zero-byte files and empty directories are accessible in the tree.

Filtering keeps matching names and their ancestors in the tree. The map keeps the full size context. Picking a block outside the filter clears the filter so the selected item can appear in the tree. Search is scoped to the directory you are viewing.

Press `F2` for **age and size filters**. Set a minimum size (for example `500MiB`
or `2GiB`) and/or “not modified for at least” a whole number of days. Both
criteria must match. Blank or zero disables a criterion; Enter applies, Esc
cancels edits, and Ctrl+R in the dialog clears both criteria. Outside the dialog,
Esc clears the name search first, then age/size filters. A queued file transfer
still takes precedence for Esc, as shown in the footer.

These filters hide excluded items from **both the tree and space map**. Matching
folders include their contents, including smaller files within a large matching
folder. Ancestors remain expanded as `[context]` so matching descendants can be
reached; the scan root itself never causes its whole contents to match. A folder’s
age is its newest modification anywhere in the scanned subtree, including the
folder itself. Unknown/incomplete modification ages do not satisfy an age filter.
Modification age describes filesystem metadata, rather than when something was
last used.

The tree’s filtered sizes/file counts and the map reflect matching contents.
The header keeps full scan totals and shows a separate active-filter banner;
selected-item details retain actual full sizes. Filters survive rescans and are
applied to the scan data without an additional scan when edited. Sizes use the
current allocated/apparent-size mode. `GB` means decimal bytes; `GiB` means binary.

Context folders cannot be marked, copied, moved, renamed or deleted while they
contain filtered-out context. Previously marked targets remain marked, but file
actions require clearing the filters or unmarking any excluded/context targets.
This avoids silently including hidden contents in a folder operation. Matching
folders remain whole filesystem targets. Git cleanup previews continue to describe
the whole selected checkout, independently of these view filters.

Name search still filters tree names while keeping the map’s current age/size
context. Manual `h`/`H` visibility choices remain separate and affect only the map;
clearing age/size filters preserves those choices.

To set aside a known large item such as `target/`, select it and press `h`. The treemap excludes its entire subtree and expands the remaining blocks to fill the panel. Hidden entries stay in the tree, dimmed and marked `[hidden]`; their actual sizes and the scan totals are unchanged. Percentages and usage bars show each item's share of the remaining map, and the map title reports the visible size and hidden-item count.

Press `h` on a hidden item to show it again, or `H` to restore everything. Showing a child also reveals any hidden ancestors, while independently hidden siblings stay hidden. Visibility choices survive rescans during the current session, including new files added within a hidden directory. Hiding items never changes files on disk.

## Git inspection, sync and cleanup

Press `Ctrl+g` on a repository or an item inside it to open the floating action
panel. This also works when scanning a directory inside a larger repository.
The **GIT OPERATIONS** panel starts with the local work to review before reclaiming
space: uncommitted changes, untracked and ignored items, commits absent from
fetched remote refs, branches without a verified remote upstream, and stashes.
The commit count covers all local branches plus detached HEAD and counts shared
commits once. A branch without an upstream can still have all its commits on a
remote; the two counts are separate. No fetched remote refs means **UNVERIFIED**.
Ignored items can be whole directories, such as `target/`, rather than individual
files; review the cleanup preview for the exact paths affected.

Clean and Reset appear before the supporting inspection and sync actions, with
an explanation of what each discards or preserves. This helps distinguish local
work worth keeping from disposable dependencies and build output. Optional
commit, branch and stash deletion stays off by default.

The summary loads only when Git Operations opens, uses local Git data, and does
not run a second disk-usage scan. Press `R` to refresh the home summary locally,
or `F` to fetch updated remote refs. Comparisons always describe cached refs,
rather than a guarantee about the current remote. Cleanup previews recheck the
repository before making changes.

The panel identifies primary repositories, bare hubs and linked worktrees, and
shows the current branch and configured upstream. A linked worktree
also offers `D` to delete that checkout directly, without opening the worktree list.
This uses the same options, preview and `y` confirmation as worktree management;
its parent repository's branch and shared history are retained.

| Panel key | Action |
| --- | --- |
| i | Show clean/dirty state, ahead/behind counts, and staged/unstaged/untracked paths |
| v | Select local-only branches and branches with unpushed commits; sync or delete |
| u | List commits on HEAD absent from fetched remote refs; push the branch |
| z | Select and delete individual repository-wide stashes |
| t | Open a shell here; exit returns to this Git panel |
| F | Fetch configured remotes and refresh Git indicators |
| R | Refresh the local work summary without network access |
| w | Open the repository’s worktree manager |
| D | Preview deletion of this linked worktree (shown only for linked worktrees) |
| p / P | Preview Pull / Push |
| c | Preview Clean checkout |
| r | Preview Reset to remote |
| f | Toggle force-with-lease for Push (default: off) |
| l | Toggle discarding current-branch local commits during Clean (default: off) |
| b | Toggle deleting other local branches during Reset (default: off) |
| s | Toggle clearing repository-wide stashes during Reset (default: off) |

Status shows checkout cleanliness and ahead/behind counts, followed by separate
Staged, Unstaged and Untracked path lists. Renames show their source and destination;
unmerged paths appear in a separate conflict section. Names are escaped safely,
file contents are not shown, and each section displays up to 1,000 paths. Counts
use the configured upstream’s last-fetched refs; missing upstreams or refs show as
unknown. Headers, labels, status values, commands and warnings use
contrasting colors in floating panels. Use arrows, PageUp/PageDown or Home/End to
scroll Status and previews; Esc or Enter returns to the Git menu.

Branches, Local commits and Stashes are selectable lists. Use arrows or
PageUp/PageDown to select, Home/End for the first/last row, and `r` to refresh.
Inspection shortcuts work across reports, and `t` opens a shell in the displayed
repository or worktree. The same menu or list refreshes when the shell returns.
Diff is available through the shell.

In **Branches**, Enter or `c` immediately checks out the selected local branch and
refreshes the list. Checkout requires the current checkout to have no staged,
unstaged, untracked or conflicted paths, and no interrupted Git operation. It
rechecks just before switching, protects ignored files from being overwritten,
and refuses branches already checked out elsewhere. Checkout does not fetch,
force, stash, or recursively update submodules.

`s` previews Sync and `d`/Delete previews deleting the selected local branch. Sync fetches its upstream, fast-forwards if possible, then pushes
pending commits. It never rebases or force-pushes. An unchecked branch can advance
without changing the current checkout; a branch checked out in another worktree
syncs there. Dirty checkouts cannot be fast-forwarded. Divergence requires the
shell. A branch without a remote upstream publishes to `origin`, or the only
configured remote; ambiguous targets require upstream configuration in the shell.
The destination appears in the preview. Checked-out branches and branches in
interrupted rebase/bisect operations cannot be deleted. Branch deletion keeps
remote branches and working files, but unpushed commits may lose their local ref.

In **Local commits**, `p` previews pushing the current branch’s pending commits,
including publishing it when no upstream exists. The list compares HEAD with all
fetched remote refs and shows at most 200 commits. Push publishes the whole
branch; it does not cherry-pick a selected commit. Detached commits need an
attached branch before pushing.

In **Stashes**, `d`/Delete previews deleting just the selected stash. Stashes are
shared across linked worktrees. Selectors and object IDs are rechecked before
deletion so a changed stash list requires a fresh preview.

Sync, Push, branch deletion and stash deletion require `y` at their confirmation
preview; Enter or Esc cancels. Lists retain selection where possible and refresh
after results. Fetch is explicit; listing and Status never contact remotes.
`F` fetches configured remotes and refreshes indicators without merging, pruning,
or fetching tags or submodules.

The main menu’s Pull, Push, Clean checkout and Reset to remote also open previews.
Preparation may fetch refs; checkout changes, deletion and pushes require `y`.
Enter, `n` or Esc cancels. Repository changes require a fresh preview.

**Pull** fetches the configured upstream and fast-forwards to its previewed commit.
Divergent histories require the shell. **Push** sends only the current branch to
its configured upstream. Force is optional and uses an explicit lease on the
remote commit shown in the preview; a concurrent remote update rejects the push.
These main-menu Pull/Push/Reset actions require an attached branch with an upstream.
Publishing branches without upstreams is available through Branches or Local commits.

**Clean checkout** restores tracked files and the index to `HEAD`, then removes
untracked and ignored files, including build output. It keeps local commits,
branches, and stashes unless the option to discard current-branch local commits
is enabled; that option restores the fetched upstream commit instead.

**Reset to remote** restores the current branch to its fetched upstream commit and
removes untracked and ignored files. Other local branches and stashes are kept by
default. The optional branch cleanup includes unpushed branches, lists their
commits in the preview, and keeps branches checked out in another worktree.
Stash cleanup affects the whole repository, including its linked worktrees.

Cleanup resets in place: Git configuration, tags, reflogs, and retained objects
remain. Removing local commits does not guarantee an immediate reduction in
`.git` storage. Nested repositories, submodules, and other worktrees are not
recursively reset or cleaned. Bare repositories and interrupted Git operations
are left for the shell.

Git jobs open the same floating progress panel, showing the current command,
elapsed time, and live transfer output when Git provides it. The panel follows
new output automatically; arrows and PageUp/PageDown scroll its recent history,
and End resumes following. Esc hides it without stopping the job; F3 reopens it.

Commands run in the background using existing credentials and SSH configuration,
without interactive password prompts. Errors and results remain in a scrollable
dialog. Use F4 to request cancellation; completed steps stay completed, and a
cancelled push may need its remote outcome checked in the shell. File mutations,
rescans, and shell launches wait while a Git job is active. For other Git work,
press `t` in Git Operations to open the shell.

## Worktree management

Press `Ctrl+g` on a repository or an item inside it, then `w` in the Git menu to
list its registered linked worktrees. Esc returns to the Git menu.
The primary checkout or bare hub is kept in the Git menu and is neither listed
nor measured here. The list includes linked checkouts outside the scanned directory.
Bare hubs also open through `Ctrl+g`; checkout actions are available by selecting
a worktree. Sizes update in the background, largest first, and include ignored
dependencies and build output.
Shared Git metadata is excluded from checkout sizes. Partial scans and unavailable
paths are marked; reported usage is not a guarantee of space that can be freed.
Worktree discovery and size measurements run only when this manager is opened,
never during the initial directory scan. Measurements pause during rescans and
Git jobs, then refresh when returning to the list.

The view shows branch or detached HEAD, locked/prunable flags, local changes,
and upstream state from last-fetched references. Each checkout has a status icon:

| Icon | Meaning |
| --- | --- |
| `✓` (green) | Clean checkout; its branch matches its configured remote upstream |
| `*` | Staged, modified, or untracked files |
| `↑N` / `↓N` | Commits ahead / behind the upstream; both appear for divergence |
| `◇` | Detached HEAD |
| `?` | No remote upstream, missing tracking ref, or unavailable status |
| `…` | Still checking |

Sync refers to this checkout's branch and **last-fetched refs**, rather than every
branch in the repository. Ignored build output does not make a checkout dirty.
Checks never fetch automatically; use `F` in its Git menu, then return to the list
to update comparisons.
Use arrows to select, `m`/`M` to mark or clear marks, `y` to copy a path, `t` to
open a shell, and `r` to refresh. Enter opens the usual Pull, Push, Clean checkout,
and Reset to remote actions for the selected checkout. Closing those dialogs
returns to the worktree list with the selection and marks preserved.

Press `d` to remove the selected or marked checkouts. Force starts off; use `f` in
the removal options to enable it explicitly, then Enter to prepare a preview.
The preview lists local changes, untracked and ignored content, detached-only
commits, and reported checkout usage. Only `y` executes; Enter or Esc cancels.
A separate `v` option, **Remove nested repositories/submodules**, starts off and
requires Force as well. With both enabled, the preview lists each nested path,
HEAD, refs and local changes, including ignored files. Removing those nested
repositories can permanently discard their own branches, stashes and unpushed
history along with their Git metadata. File contents are never shown in previews.
Associated local branches are kept, including unpushed branches. Force is required
for local changes and detached commits not retained by branches, tags, or
remote-tracking references.

Main worktrees, bare hubs, locked worktrees, and checkouts containing Spacewalk's
scan root or process working directory cannot be removed. Nested repositories and
submodules are protected unless the separate nested-removal option is enabled.
Nested linked/locked worktrees and repositories supporting checkouts outside the
selected tree remain protected even with both options enabled. Registrations,
nested refs and checkout contents are checked again before removal; partial
batches retain unsuccessful selections.
If Git unregisters a checkout but leaves files behind after an I/O error, its
leftover path remains marked in the open manager for inspection in the shell.
Progress and results use the usual floating panels, and the list refreshes when
you return.

Press `p` for a separate stale-registration prune preview. Pruning removes eligible
administrative records, not existing checkout folders, branches, or Git objects.
It honors locks and `gc.worktreePruneExpire` (default `3.months.ago`), so a newly
missing checkout may still be retained. Pruning also requires `y` confirmation.
There is no automatic fetching, branch deletion, unlocking, or forced object
pruning in worktree management. Use Enter for the existing Git actions and their
separate options.

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

The code separates filesystem scanning (`scan`), navigation state (`browser`), weighted map layout (`treemap`), Git checks (`git`), selectable reports and branch/stash actions (`git_manage`), clipboard copying (`clipboard`), deletion (`delete`), event handling (`app`), and rendering (`ui`). The interface uses [Ratatui](https://docs.rs/ratatui/0.30.2/ratatui/) and [Crossterm](https://docs.rs/crossterm/0.29.0/crossterm/).

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
