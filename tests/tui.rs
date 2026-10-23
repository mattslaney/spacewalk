use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use spacewalk::{
    app::{App, Modal},
    browser::Panel,
    git::{RemoteInfo, RepoState, RepoStatus},
    scan::Options,
    ui,
};
use std::{
    fs,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn key(app: &mut App, code: KeyCode) {
    app.key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn wait(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.scanning.is_some()
        || app.deleting.is_some()
        || app.file_task.is_some()
        || app.git_action.is_some()
    {
        assert!(
            Instant::now() < deadline,
            "worker timed out: {}",
            app.status
        );
        app.poll_workers();
        thread::sleep(Duration::from_millis(1));
    }
}

fn fixture() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("photos/nested")).unwrap();
    fs::write(dir.path().join("photos/nested/large.jpg"), [0; 6000]).unwrap();
    fs::write(dir.path().join("archive.zip"), [0; 3000]).unwrap();
    fs::write(dir.path().join("notes.txt"), [0; 1000]).unwrap();
    let mut app = App::new(
        dir.path().to_owned(),
        Options {
            apparent_size: true,
        },
    );
    wait(&mut app);
    (dir, app)
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn keyboard_and_mouse_link_both_panels_and_search() {
    let (_dir, mut app) = fixture();
    let screen = render(&mut app, 120, 42);
    assert!(screen.contains("FILE TREE"));
    assert!(screen.contains("SPACE MAP"));
    assert!(screen.contains("9.8 KiB"));
    key(&mut app, KeyCode::Down);
    let selected_dir = app.browser.as_ref().unwrap().selected;
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.browser.as_ref().unwrap().focus, Panel::Map);
    assert_eq!(app.browser.as_ref().unwrap().selected, selected_dir);
    key(&mut app, KeyCode::Right);
    let b = app.browser.as_ref().unwrap();
    let selected = b.selected;
    assert!(b.rows.iter().any(|row| row.id == selected));
    assert!(b.map.leaves.iter().any(|tile| tile.id == selected));
    let tile = *b
        .map
        .leaves
        .iter()
        .find(|tile| tile.id != selected)
        .unwrap();
    app.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: tile.rect.x,
        row: tile.rect.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.browser.as_ref().unwrap().selected, tile.id);
    assert!(
        app.browser
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|row| row.id == tile.id)
    );
    render(&mut app, 120, 42);
    let &(y, id, x) = app
        .hits
        .rows
        .iter()
        .find(|(_, id, _)| *id != tile.id)
        .unwrap();
    app.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x + 2,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.browser.as_ref().unwrap().selected, id);
    assert_eq!(app.browser.as_ref().unwrap().focus, Panel::Tree);
    key(&mut app, KeyCode::Char('/'));
    for c in "jpg".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    assert_eq!(
        app.browser.as_ref().unwrap().data.nodes[app.browser.as_ref().unwrap().selected].name,
        "large.jpg"
    );
    key(&mut app, KeyCode::Enter);
    assert!(!app.searching);
    let matched = app.browser.as_ref().unwrap().selected;
    key(&mut app, KeyCode::Esc);
    assert!(app.browser.as_ref().unwrap().query.is_empty());
    assert_eq!(app.browser.as_ref().unwrap().selected, matched);
}

#[test]
fn rescan_restores_view_and_delete_requires_explicit_confirmation() {
    let (dir, mut app) = fixture();
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('s'));
    let scope = app
        .browser
        .as_ref()
        .unwrap()
        .data
        .path(app.browser.as_ref().unwrap().scope);
    fs::write(dir.path().join("new.txt"), b"new").unwrap();
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app);
    let b = app.browser.as_ref().unwrap();
    assert_eq!(b.data.path(b.scope), scope);
    assert_eq!(b.sort.label(), "name ↑");
    key(&mut app, KeyCode::Backspace);
    let b = app.browser.as_mut().unwrap();
    let id = b.data.find_path(&dir.path().join("notes.txt")).unwrap();
    b.select(id);
    key(&mut app, KeyCode::Char('d'));
    assert!(matches!(app.modal, Some(Modal::Delete(_))));
    assert!(render(&mut app, 100, 30).contains("CONFIRM PERMANENT DELETION"));
    assert!(render(&mut app, 60, 20).contains("[y] Delete permanently"));
    key(&mut app, KeyCode::Enter);
    assert!(dir.path().join("notes.txt").exists());
    key(&mut app, KeyCode::Delete);
    key(&mut app, KeyCode::Char('y'));
    wait(&mut app);
    assert!(!dir.path().join("notes.txt").exists());
    assert!(
        app.browser
            .as_ref()
            .unwrap()
            .data
            .find_path(&dir.path().join("notes.txt"))
            .is_none()
    );
    assert_eq!(app.browser.as_ref().unwrap().data.nodes[0].size, 9003);
    assert!(dir.path().join("archive.zip").exists());
}

#[test]
fn renders_small_terminals_empty_search_and_modals_without_panicking() {
    let (_dir, mut app) = fixture();
    for (width, height) in [(1, 1), (50, 12), (60, 20), (80, 24), (120, 42)] {
        render(&mut app, width, height);
        app.modal = Some(Modal::Help);
        render(&mut app, width, height);
        app.modal = None;
    }
    app.browser.as_mut().unwrap().filter("not found".into());
    assert!(render(&mut app, 80, 24).contains("No matches"));
    app.modal = Some(Modal::Errors);
    assert!(render(&mut app, 80, 24).contains("No scan errors"));
    app.modal = None;
    key(&mut app, KeyCode::Char('q'));
    assert!(app.quitting);
}

#[test]
fn summary_cli_reports_sizes_and_invalid_roots_fail() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("test.bin"), [0; 321]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_spacewalk"))
        .args(["--summary", "--apparent-size"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("321 B"));
    assert!(text.contains("test.bin"));
    let output = Command::new(env!("CARGO_BIN_EXE_spacewalk"))
        .arg("--summary")
        .arg(dir.path().join("missing"))
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn quit_finishes_a_deletion_already_in_progress() {
    let (_dir, mut app) = fixture();
    let (sender, receiver) = std::sync::mpsc::channel();
    app.deleting = Some(receiver);
    key(&mut app, KeyCode::Char('q'));
    assert!(!app.quitting);
    sender.send(Ok(())).unwrap();
    assert!(app.poll_workers());
    assert!(app.quitting);
    assert!(app.scanning.is_none());
}

#[test]
fn worker_updates_request_a_redraw_when_a_scan_finishes_or_fails() {
    let (_dir, mut app) = fixture();
    assert!(!app.poll_workers());
    app.rescan();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.scanning.is_some() {
        assert!(Instant::now() < deadline);
        let changed = app.poll_workers();
        if app.scanning.is_none() {
            assert!(changed, "scan completion must update the screen");
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(!app.poll_workers());

    app.root.push("missing");
    app.rescan();
    while app.scanning.is_some() {
        assert!(Instant::now() < deadline);
        let changed = app.poll_workers();
        if app.scanning.is_none() {
            assert!(changed, "scan failure must update the screen");
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(app.status.starts_with("Scan failed"));
    assert!(!app.poll_workers());
}

#[test]
fn clipboard_completion_updates_feedback_while_navigation_stays_available() {
    let (dir, mut app) = fixture();
    let (sender, receiver) = std::sync::mpsc::channel();
    app.copying = Some(receiver);
    app.status = "Clipboard: copying selected path…".into();
    assert!(!app.poll_workers());
    let previous = app.browser.as_ref().unwrap().selected;
    key(&mut app, KeyCode::Down);
    assert_ne!(app.browser.as_ref().unwrap().selected, previous);
    sender.send(Ok(dir.path().join("notes.txt"))).unwrap();
    assert!(app.poll_workers());
    assert!(app.copying.is_none());
    let screen = render(&mut app, 120, 42);
    assert!(screen.contains("Clipboard: copied"));
    assert!(screen.contains("notes.txt"));

    let (sender, receiver) = std::sync::mpsc::channel();
    app.copying = Some(receiver);
    sender
        .send(Err("No desktop clipboard session is available".into()))
        .unwrap();
    assert!(app.poll_workers());
    assert!(app.copying.is_none());
    assert!(render(&mut app, 60, 20).contains("Clipboard unavailable:"));
    assert!(!app.poll_workers());
}

#[test]
fn copying_requires_a_selection_and_does_not_interfere_with_search_or_modals() {
    let (_dir, mut app) = fixture();
    let mut repeat = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    app.key(repeat);
    assert!(app.copying.is_none());
    app.browser.as_mut().unwrap().filter("no-match".into());
    key(&mut app, KeyCode::Char('y'));
    assert!(app.copying.is_none());
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('y'));
    assert_eq!(app.browser.as_ref().unwrap().query, "y");
    assert!(app.copying.is_none());
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('?'));
    key(&mut app, KeyCode::Char('y'));
    assert!(app.copying.is_none());
    app.modal_scroll = 10;
    assert!(render(&mut app, 120, 60).contains("Copy selected path to clipboard"));
}

#[test]
fn git_icons_and_colors_distinguish_sync_changes_and_missing_remotes() {
    let (_dir, mut app) = fixture();
    let repo = app.browser.as_ref().unwrap().selected;
    for (state, icons, detail, color) in [
        (
            RepoState::Checking,
            "⎇ …",
            "checking repository",
            ratatui::style::Color::Rgb(133, 151, 174),
        ),
        (
            RepoState::Ready(RepoStatus {
                branches: 1,
                has_remote: true,
                fetched: true,
                ..RepoStatus::default()
            }),
            "⎇ ✓",
            "every local branch matches",
            ratatui::style::Color::Rgb(107, 226, 188),
        ),
        (
            RepoState::Ready(RepoStatus::default()),
            "⎇ ∅",
            "no configured remote",
            ratatui::style::Color::Rgb(180, 155, 225),
        ),
        (
            RepoState::Ready(RepoStatus {
                dirty: true,
                ahead: 1,
                behind: 1,
                branches: 1,
                has_remote: true,
                fetched: true,
                ..RepoStatus::default()
            }),
            "⎇ *↑↓",
            "working tree changes",
            ratatui::style::Color::Rgb(250, 188, 111),
        ),
        (
            RepoState::Unavailable("Git missing".into()),
            "⎇ ?",
            "status unavailable",
            ratatui::style::Color::Rgb(250, 188, 111),
        ),
    ] {
        app.browser
            .as_mut()
            .unwrap()
            .repositories
            .insert(repo, state);
        for (width, height) in [(60, 20), (120, 42)] {
            let screen = render(&mut app, width, height);
            assert!(
                screen.contains(icons),
                "missing {icons} at {width}x{height}"
            );
            assert!(screen.contains(detail));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let &(y, _, twisty) = app.hits.rows.iter().find(|(_, id, _)| *id == repo).unwrap();
            let cell = &terminal.backend().buffer()[(twisty + 2, y)];
            assert_eq!(cell.symbol(), "⎇");
            assert_eq!(cell.fg, color);
        }
    }
}

#[test]
fn selected_repository_displays_its_remote_url_before_status_details() {
    let (_dir, mut app) = fixture();
    let b = app.browser.as_mut().unwrap();
    b.repositories.insert(
        b.selected,
        RepoState::Ready(RepoStatus {
            branches: 1,
            has_remote: true,
            fetched: true,
            remote: Some(RemoteInfo {
                name: "upstream".into(),
                url: "git@github.com:team/repo.git".into(),
                upstream: true,
            }),
            ..RepoStatus::default()
        }),
    );
    for (width, height) in [(60, 20), (120, 42)] {
        let screen = render(&mut app, width, height);
        assert!(screen.contains("Git: git@github.com:team/repo.git"));
        assert!(screen.contains("upstream upstream"));
        assert!(screen.contains("⎇ ✓"));
    }
    let b = app.browser.as_mut().unwrap();
    let RepoState::Ready(status) = b.repositories.get_mut(&b.selected).unwrap() else {
        unreachable!()
    };
    status.remote.as_mut().unwrap().url =
        format!("https://host/{}/repo.git", "long-namespace/".repeat(10));
    let screen = render(&mut app, 60, 20);
    let line = screen
        .lines()
        .find(|line| line.contains("Git: https://host/"))
        .unwrap();
    assert!(line.contains('…'));
}

#[test]
fn background_git_checks_refresh_after_rescans_and_drop_removed_repositories() {
    fn wait_git(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.git.is_some() {
            assert!(Instant::now() < deadline, "Git worker stalled");
            app.poll_workers();
            thread::sleep(Duration::from_millis(1));
        }
    }
    let (dir, mut app) = fixture();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let output = Command::new("git").arg("init").arg(&repo).output().unwrap();
    assert!(output.status.success());
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app);
    let id = app.browser.as_ref().unwrap().data.find_path(&repo).unwrap();
    assert!(app.browser.as_ref().unwrap().repositories.contains_key(&id));
    // Tree input is available while the separate Git worker runs.
    key(&mut app, KeyCode::Down);
    wait_git(&mut app);
    let RepoState::Ready(status) = &app.browser.as_ref().unwrap().repositories[&id] else {
        panic!("Git check did not complete")
    };
    assert!(!status.has_remote);
    assert!(!app.poll_workers());
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args([
            "remote",
            "add",
            "origin",
            "https://example.invalid/unreachable.git",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app);
    wait_git(&mut app);
    let id = app.browser.as_ref().unwrap().data.find_path(&repo).unwrap();
    let RepoState::Ready(status) = &app.browser.as_ref().unwrap().repositories[&id] else {
        panic!("Git recheck did not complete")
    };
    assert!(status.has_remote);
    assert!(!status.fetched);
    fs::remove_dir_all(repo.join(".git")).unwrap();
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app);
    assert!(app.git.is_none());
    assert!(app.browser.as_ref().unwrap().repositories.is_empty());
}

#[test]
fn visibility_keys_reflow_the_map_and_survive_rescan_with_new_node_ids() {
    let (dir, mut app) = fixture();
    key(&mut app, KeyCode::Down);
    let old_photos = app.browser.as_ref().unwrap().selected;
    key(&mut app, KeyCode::Char('h'));
    let screen = render(&mut app, 120, 42);
    assert!(screen.contains("[hidden] photos/"));
    assert!(screen.contains("3.9 KiB shown"));
    assert!(screen.contains("1 hidden [H show all]"));
    let b = app.browser.as_ref().unwrap();
    assert_eq!(b.data.nodes[0].size, 10000);
    assert_eq!(b.visible_size(0), 4000);
    assert!(
        b.map
            .leaves
            .iter()
            .all(|tile| !b.data.is_within(tile.id, old_photos))
    );
    let area = app.hits.map;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            assert!(b.map.hit(x, y).is_some());
        }
    }
    // Adding an earlier-sorted name shifts arena IDs. Visibility follows paths.
    fs::write(dir.path().join("aaa.txt"), [0; 1000]).unwrap();
    fs::write(dir.path().join("photos/new.jpg"), [0; 10]).unwrap();
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app);
    let b = app.browser.as_ref().unwrap();
    let photos = b.data.find_path(&dir.path().join("photos")).unwrap();
    assert_ne!(photos, old_photos);
    assert!(b.is_hidden(photos));
    assert_eq!(b.visible_size(0), 5000);
    assert_eq!(
        fs::metadata(dir.path().join("photos/nested/large.jpg"))
            .unwrap()
            .len(),
        6000
    );
    key(&mut app, KeyCode::Char('h'));
    assert!(!app.browser.as_ref().unwrap().is_hidden(photos));
    key(&mut app, KeyCode::Char('h'));
    key(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 60, 20);
    assert!(screen.contains("No visible space"));
    key(&mut app, KeyCode::Char('H'));
    render(&mut app, 120, 42);
    assert_eq!(app.browser.as_ref().unwrap().hidden_items().count(), 0);
    assert_eq!(app.browser.as_ref().unwrap().visible_size(0), 11010);
    let tile = app.browser.as_ref().unwrap().map.leaves[0];
    app.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: tile.rect.x,
        row: tile.rect.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.browser.as_ref().unwrap().selected, tile.id);
    key(&mut app, KeyCode::Char('h'));
    render(&mut app, 120, 42);
    assert!(app.browser.as_ref().unwrap().is_hidden(tile.id));
    assert!(
        app.browser
            .as_ref()
            .unwrap()
            .map
            .leaves
            .iter()
            .all(|t| t.id != tile.id)
    );
    key(&mut app, KeyCode::Char('H'));
    render(&mut app, 120, 42);
    assert!(
        app.browser
            .as_ref()
            .unwrap()
            .map
            .leaves
            .iter()
            .any(|t| t.id == tile.id)
    );
}

#[test]
fn marked_items_are_confirmed_together_even_outside_the_current_filter() {
    let (dir, mut app) = fixture();
    for (index, name) in ["archive.zip", "notes.txt"].iter().enumerate() {
        let b = app.browser.as_mut().unwrap();
        let id = b.data.find_path(&dir.path().join(name)).unwrap();
        b.select(id);
        if index == 1 {
            key(&mut app, KeyCode::Tab);
        }
        key(&mut app, KeyCode::Char('m'));
    }
    assert_eq!(app.browser.as_ref().unwrap().marked.len(), 2);
    let screen = render(&mut app, 120, 42);
    assert!(screen.contains("× archive.zip"));
    assert!(screen.contains("× notes.txt"));
    assert!(screen.contains("2 marked"));
    key(&mut app, KeyCode::Char('m'));
    assert_eq!(app.browser.as_ref().unwrap().marked.len(), 1);
    key(&mut app, KeyCode::Char('m'));
    app.browser.as_mut().unwrap().filter("no-match".into());
    key(&mut app, KeyCode::Delete);
    let Some(Modal::Delete(batch)) = &app.modal else {
        panic!("batch confirmation missing")
    };
    assert_eq!(batch.items.len(), 2);
    assert_eq!(batch.size, 4000);
    assert!(render(&mut app, 60, 20).contains("Delete these 2 items?"));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.browser.as_ref().unwrap().marked.len(), 2);
    assert!(dir.path().join("archive.zip").exists());
    assert!(dir.path().join("notes.txt").exists());
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::Char('y'));
    wait(&mut app);
    assert!(!dir.path().join("archive.zip").exists());
    assert!(!dir.path().join("notes.txt").exists());
    assert!(dir.path().join("photos/nested/large.jpg").exists());
    assert!(app.browser.as_ref().unwrap().marked.is_empty());
}

#[test]
fn directory_marks_cover_children_once_and_can_be_cleared_without_deletion() {
    let (dir, mut app) = fixture();
    for name in ["photos", "photos/nested/large.jpg"] {
        let b = app.browser.as_mut().unwrap();
        let id = b.data.find_path(&dir.path().join(name)).unwrap();
        b.select(id);
        key(&mut app, KeyCode::Char('m'));
    }
    key(&mut app, KeyCode::Char('d'));
    let Some(Modal::Delete(batch)) = &app.modal else {
        panic!("confirmation missing")
    };
    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.items[0].path, dir.path().join("photos"));
    assert_eq!(batch.size, 6000);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.browser.as_ref().unwrap().marked.len(), 2);
    key(&mut app, KeyCode::Char('M'));
    assert!(app.browser.as_ref().unwrap().marked.is_empty());
    assert!(dir.path().join("photos/nested/large.jpg").exists());
}

#[test]
fn marks_survive_navigation_and_rescans_but_not_replacement_items() {
    let (dir, mut app) = fixture();
    let notes = dir.path().join("notes.txt");
    let b = app.browser.as_mut().unwrap();
    let old_id = b.data.find_path(&notes).unwrap();
    b.select(old_id);
    key(&mut app, KeyCode::Char('m'));
    key(&mut app, KeyCode::Char('s'));
    app.browser.as_mut().unwrap().filter("large".into());
    key(&mut app, KeyCode::Esc);
    let b = app.browser.as_mut().unwrap();
    let photos = b.data.find_path(&dir.path().join("photos")).unwrap();
    b.select(photos);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Backspace);
    fs::write(dir.path().join("aaa.txt"), b"new").unwrap();
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app);
    let b = app.browser.as_ref().unwrap();
    let new_id = b.data.find_path(&notes).unwrap();
    assert_ne!(old_id, new_id);
    assert_eq!(b.marked.iter().copied().collect::<Vec<_>>(), vec![new_id]);
    fs::rename(&notes, dir.path().join("old-notes.txt")).unwrap();
    fs::write(&notes, b"replacement").unwrap();
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app);
    assert!(app.browser.as_ref().unwrap().marked.is_empty());
    assert_eq!(fs::read(notes).unwrap(), b"replacement");
}

#[test]
fn hide_uses_h_while_left_arrow_collapses_and_root_cannot_be_marked() {
    let (_dir, mut app) = fixture();
    key(&mut app, KeyCode::Char('m'));
    assert!(app.browser.as_ref().unwrap().marked.is_empty());
    assert!(render(&mut app, 120, 42).contains("cannot be marked"));
    for key_code in ['v', 'V'] {
        key(&mut app, KeyCode::Char(key_code));
    }
    assert_eq!(app.browser.as_ref().unwrap().hidden_items().count(), 0);
    key(&mut app, KeyCode::Char('h'));
    let b = app.browser.as_ref().unwrap();
    assert!(b.is_hidden(0));
    assert!(b.expanded.contains(&0));
    key(&mut app, KeyCode::Char('H'));
    assert!(!app.browser.as_ref().unwrap().is_hidden(0));
    key(&mut app, KeyCode::Left);
    assert!(!app.browser.as_ref().unwrap().expanded.contains(&0));
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('m'));
    let mut repeat = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE);
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    app.key(repeat);
    assert_eq!(app.browser.as_ref().unwrap().marked.len(), 1);
}

#[test]
fn terminal_binding_uses_the_selected_directory_or_file_parent_and_preserves_marks() {
    let (dir, mut app) = fixture();
    let b = app.browser.as_mut().unwrap();
    let photos = b.data.find_path(&dir.path().join("photos")).unwrap();
    b.select(photos);
    key(&mut app, KeyCode::Char('m'));
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(
        app.shell.take().unwrap().directory,
        dir.path().join("photos")
    );
    assert_eq!(app.browser.as_ref().unwrap().selected, photos);
    assert!(app.browser.as_ref().unwrap().marked.contains(&photos));
    let b = app.browser.as_mut().unwrap();
    let file = b
        .data
        .find_path(&dir.path().join("photos/nested/large.jpg"))
        .unwrap();
    b.select(file);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(
        app.shell.take().unwrap().directory,
        dir.path().join("photos/nested")
    );
    assert_eq!(app.browser.as_ref().unwrap().selected, file);
    app.shell_finished(Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "shell missing",
    )));
    assert!(render(&mut app, 120, 42).contains("Terminal unavailable: shell missing"));
    assert!(app.browser.as_ref().unwrap().marked.contains(&photos));
    assert!(app.scanning.is_none());
}

#[test]
fn terminal_binding_ignores_search_modals_repeats_and_empty_selection() {
    let (_dir, mut app) = fixture();
    let mut repeat = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    app.key(repeat);
    assert!(app.shell.is_none());
    key(&mut app, KeyCode::Char('?'));
    key(&mut app, KeyCode::Char('t'));
    assert!(app.shell.is_none());
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.browser.as_ref().unwrap().query, "t");
    assert!(app.shell.is_none());
    key(&mut app, KeyCode::Esc);
    app.browser.as_mut().unwrap().filter("no-match".into());
    key(&mut app, KeyCode::Char('t'));
    assert!(app.shell.is_none());
}

#[test]
fn terminal_waits_for_active_scan_or_deletion() {
    let (_dir, mut app) = fixture();
    let (_sender, receiver) = std::sync::mpsc::channel();
    app.deleting = Some(receiver);
    key(&mut app, KeyCode::Char('t'));
    assert!(app.shell.is_none());
    assert!(app.status.starts_with("Wait for"));
    app.deleting = None;
    app.rescan();
    key(&mut app, KeyCode::Char('t'));
    assert!(app.shell.is_none());
    assert!(app.status.starts_with("Wait for"));
    wait(&mut app);
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        key(app, KeyCode::Char(ch));
    }
}

#[test]
fn file_dialogs_create_rename_copy_and_move_without_using_clipboard() {
    let (dir, mut app) = fixture();
    app.browser.as_mut().unwrap().select(0);
    key(&mut app, KeyCode::Char('a'));
    type_text(&mut app, "new café");
    assert!(render(&mut app, 60, 20).contains("NEW FOLDER"));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(dir.path().join("new café").is_dir());
    assert!(matches!(app.modal, Some(Modal::Result { .. })));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('R'));
    app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    type_text(&mut app, "renamed");
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(dir.path().join("renamed").is_dir());
    key(&mut app, KeyCode::Esc);
    let file = app
        .browser
        .as_ref()
        .unwrap()
        .data
        .find_path(&dir.path().join("notes.txt"))
        .unwrap();
    app.browser.as_mut().unwrap().select(file);
    key(&mut app, KeyCode::Char('c'));
    assert!(app.copying.is_none());
    assert!(app.transfer.is_some());
    let dest = app
        .browser
        .as_ref()
        .unwrap()
        .data
        .find_path(&dir.path().join("renamed"))
        .unwrap();
    app.browser.as_mut().unwrap().select(dest);
    key(&mut app, KeyCode::Char('p'));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(dir.path().join("renamed/notes.txt").exists());
    assert!(app.transfer.is_some());
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Esc);
    assert!(app.transfer.is_none());
    key(&mut app, KeyCode::Char('x'));
    app.browser.as_mut().unwrap().select(0);
    key(&mut app, KeyCode::Char('p'));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    // The original notes.txt is a collision: neither copy is changed.
    assert!(dir.path().join("notes.txt").exists());
    assert!(dir.path().join("renamed/notes.txt").exists());
    assert!(app.transfer.is_some());
    assert!(render(&mut app, 120, 42).contains("already exists"));
}

#[test]
fn file_actions_route_through_search_and_forms_and_respect_root_and_busy_state() {
    let (_dir, mut app) = fixture();
    key(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "cxyapR");
    assert_eq!(app.browser.as_ref().unwrap().query, "cxyapR");
    assert!(app.transfer.is_none());
    key(&mut app, KeyCode::Esc);
    app.browser.as_mut().unwrap().select(0);
    key(&mut app, KeyCode::Char('x'));
    assert!(app.transfer.is_none());
    assert!(app.status.contains("root"));
    key(&mut app, KeyCode::Char('R'));
    assert!(app.modal.is_none());
    key(&mut app, KeyCode::Char('a'));
    type_text(&mut app, "q?y");
    assert!(matches!(&app.modal, Some(Modal::FileInput(input)) if input.text == "q?y"));
    key(&mut app, KeyCode::Esc);
    app.rescan();
    key(&mut app, KeyCode::Char('a'));
    assert!(app.modal.is_none());
    assert!(app.status.starts_with("Wait for"));
    wait(&mut app);
}

#[test]
fn paste_dialog_completes_paths_and_unicode_editing_stays_on_character_boundaries() {
    let (dir, mut app) = fixture();
    let file = app
        .browser
        .as_ref()
        .unwrap()
        .data
        .find_path(&dir.path().join("notes.txt"))
        .unwrap();
    app.browser.as_mut().unwrap().select(file);
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('p'));
    app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    type_text(&mut app, "pho");
    key(&mut app, KeyCode::Tab);
    assert!(matches!(&app.modal, Some(Modal::FileInput(input)) if input.text == "photos/"));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('a'));
    type_text(&mut app, "aé文");
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Delete);
    assert!(matches!(&app.modal, Some(Modal::FileInput(input)) if input.text == "a"));
    for (width, height) in [(60, 20), (120, 42), (30, 10)] {
        let _ = render(&mut app, width, height);
    }
}

fn init_git_fixture(path: &std::path::Path) {
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "Spacewalk Test"],
        vec!["config", "user.email", "test@example.invalid"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "."],
        vec!["commit", "-m", "initial"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn open_git(app: &mut App) {
    app.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    wait(app);
    assert!(matches!(app.modal, Some(Modal::GitPanel(_))));
}

#[test]
fn git_panel_options_default_off_and_remote_actions_explain_missing_upstream() {
    let (dir, mut app) = fixture();
    init_git_fixture(dir.path());
    app.rescan();
    wait(&mut app);
    open_git(&mut app);
    assert!(
        matches!(&app.modal, Some(Modal::GitPanel(panel)) if !panel.options.force && !panel.options.discard_commits && !panel.options.delete_branches && !panel.options.clear_stashes)
    );
    key(&mut app, KeyCode::Char('p'));
    assert!(app.git_action.is_none());
    assert!(
        matches!(&app.modal, Some(Modal::GitPanel(panel)) if panel.error.as_ref().unwrap().contains("upstream"))
    );
    for option in ['f', 'l', 'b', 's'] {
        key(&mut app, KeyCode::Char(option));
    }
    assert!(
        matches!(&app.modal, Some(Modal::GitPanel(panel)) if panel.options.force && panel.options.discard_commits && panel.options.delete_branches && panel.options.clear_stashes)
    );
    key(&mut app, KeyCode::PageDown);
    for (width, height) in [(60, 20), (120, 42), (30, 10)] {
        let _ = render(&mut app, width, height);
    }
    key(&mut app, KeyCode::Esc);
    wait(&mut app);
    open_git(&mut app);
    assert!(
        matches!(&app.modal, Some(Modal::GitPanel(panel)) if !panel.options.force && !panel.options.discard_commits && !panel.options.delete_branches && !panel.options.clear_stashes)
    );
}

#[test]
fn git_reports_stay_in_the_panel_and_return_to_the_same_menu_options() {
    use spacewalk::git_actions::GitCommand;
    let (dir, mut app) = fixture();
    init_git_fixture(dir.path());
    fs::write(
        dir.path().join("report.txt"),
        "new staged line\n".repeat(200),
    )
    .unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", "report.txt"])
            .status()
            .unwrap()
            .success()
    );
    app.rescan();
    wait(&mut app);
    open_git(&mut app);
    key(&mut app, KeyCode::Char('f'));
    for (shortcut, command) in [
        ('i', GitCommand::Status),
        ('d', GitCommand::Diff),
        ('v', GitCommand::Branches),
        ('u', GitCommand::LocalCommits),
        ('z', GitCommand::Stashes),
        ('F', GitCommand::Fetch),
    ] {
        key(&mut app, KeyCode::Char(shortcut));
        wait(&mut app);
        assert!(
            matches!(&app.modal, Some(Modal::GitPanel(panel)) if panel.options.force && panel.report.as_ref().is_some_and(|(c, _)| *c == command))
        );
        assert_eq!(app.modal_scroll, 0);
        for (width, height) in [(120, 42), (60, 20), (30, 10)] {
            let screen = render(&mut app, width, height);
            if width >= 60 && height >= 20 {
                assert!(screen.contains(command.label()));
            } else {
                assert!(screen.contains("Resize to at least"));
            }
        }
        key(&mut app, KeyCode::PageDown);
        let _ = render(&mut app, 60, 20);
        if command == GitCommand::Diff {
            key(&mut app, KeyCode::End);
            let _ = render(&mut app, 60, 20);
            let end = app.modal_scroll;
            assert!(end > 0 && end < u16::MAX);
            key(&mut app, KeyCode::Up);
            assert_eq!(app.modal_scroll, end - 1);
        }
        for action in ['c', 'r', 'p', 'P', 'y', 'f'] {
            key(&mut app, KeyCode::Char(action));
        }
        assert!(app.git_action.is_none());
        assert!(dir.path().join("report.txt").exists());
        key(&mut app, KeyCode::Esc);
        assert!(
            matches!(&app.modal, Some(Modal::GitPanel(panel)) if panel.report.is_none() && panel.options.force)
        );
        assert_eq!(app.modal_scroll, 0);
    }
    key(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    wait(&mut app);
}

#[test]
fn failed_fetch_returns_to_the_git_menu_and_inspection_shortcuts_work_in_reports() {
    let (dir, mut app) = fixture();
    init_git_fixture(dir.path());
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["remote", "add", "origin", "./missing-remote.git"])
            .status()
            .unwrap()
            .success()
    );
    app.rescan();
    wait(&mut app);
    open_git(&mut app);
    key(&mut app, KeyCode::Char('F'));
    wait(&mut app);
    assert!(
        matches!(&app.modal, Some(Modal::GitPanel(panel)) if panel.report.as_ref().unwrap().1.contains("failed"))
    );
    key(&mut app, KeyCode::Char('i'));
    wait(&mut app);
    assert!(
        matches!(&app.modal, Some(Modal::GitPanel(panel)) if panel.report.as_ref().unwrap().1.contains("Branch: main"))
    );
    key(&mut app, KeyCode::Enter);
    assert!(matches!(&app.modal, Some(Modal::GitPanel(panel)) if panel.report.is_none()));
}

#[test]
fn git_cleanup_requires_explicit_confirmation_and_refreshes_the_view() {
    let (dir, mut app) = fixture();
    init_git_fixture(dir.path());
    fs::write(dir.path().join("notes.txt"), b"changed").unwrap();
    fs::write(dir.path().join("garbage"), b"delete").unwrap();
    app.rescan();
    wait(&mut app);
    app.browser.as_mut().unwrap().select(0);
    open_git(&mut app);
    key(&mut app, KeyCode::Char('c'));
    wait(&mut app);
    assert!(matches!(app.modal, Some(Modal::GitPreview(_))));
    assert!(render(&mut app, 60, 20).contains("[y] Execute action"));
    let mut repeat = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    app.key(repeat);
    assert!(app.git_action.is_none());
    assert!(dir.path().join("garbage").exists());
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(app.modal.is_none());
    assert!(dir.path().join("garbage").exists());
    open_git(&mut app);
    key(&mut app, KeyCode::Char('c'));
    wait(&mut app);
    key(&mut app, KeyCode::Char('y'));
    wait(&mut app);
    assert!(app.copying.is_none());
    assert!(!dir.path().join("garbage").exists());
    assert_eq!(fs::read(dir.path().join("notes.txt")).unwrap().len(), 1000);
    assert!(
        matches!(&app.modal, Some(Modal::Result { text, .. }) if text.contains("Clean checkout complete"))
    );
    assert!(
        app.browser
            .as_ref()
            .unwrap()
            .data
            .find_path(&dir.path().join("garbage"))
            .is_none()
    );
}

#[test]
fn git_jobs_block_other_mutations_and_quit_cancels_safely() {
    let (dir, mut app) = fixture();
    init_git_fixture(dir.path());
    app.rescan();
    wait(&mut app);
    app.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert!(app.git_action.is_some());
    assert!(matches!(app.modal, Some(Modal::Progress)));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('a'));
    assert!(app.modal.is_none());
    assert!(app.status.starts_with("Wait for"));
    key(&mut app, KeyCode::Char('d'));
    assert!(app.deleting.is_none());
    key(&mut app, KeyCode::Char('t'));
    assert!(app.shell.is_none());
    key(&mut app, KeyCode::Char('q'));
    assert!(!app.quitting);
    wait(&mut app);
    assert!(app.quitting);
}

#[test]
fn floating_progress_opens_hides_reopens_and_yields_to_the_result() {
    let (dir, mut app) = fixture();
    app.browser.as_mut().unwrap().select(0);
    key(&mut app, KeyCode::Char('a'));
    type_text(&mut app, "progress-folder");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Progress)));
    let screen = render(&mut app, 60, 20);
    assert!(screen.contains("OPERATION PROGRESS"));
    assert!(screen.contains("Create folder"));
    assert!(screen.contains("elapsed"));
    assert!(screen.contains("F4 cancel"));
    key(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert!(app.file_task.is_some());
    assert!(render(&mut app, 120, 42).contains("F3 progress"));
    key(&mut app, KeyCode::F(3));
    assert!(matches!(app.modal, Some(Modal::Progress)));
    let operation = app.operation.as_mut().unwrap();
    for n in 0..40 {
        operation.record(&format!("progress line {n}"));
    }
    assert!(render(&mut app, 60, 20).contains("progress line 39"));
    key(&mut app, KeyCode::PageUp);
    assert!(!render(&mut app, 60, 20).contains("progress line 39"));
    key(&mut app, KeyCode::End);
    assert!(render(&mut app, 60, 20).contains("progress line 39"));
    for (width, height) in [(60, 20), (120, 42), (30, 10)] {
        let _ = render(&mut app, width, height);
    }
    wait(&mut app);
    assert!(app.operation.is_none());
    assert!(matches!(app.modal, Some(Modal::Result { .. })));
    assert!(dir.path().join("progress-folder").exists());
}

#[test]
fn progress_panel_cancels_with_f4_and_does_not_route_action_shortcuts_to_the_browser() {
    let (_dir, mut app) = fixture();
    key(&mut app, KeyCode::Char('a'));
    type_text(&mut app, "fixture-folder");
    key(&mut app, KeyCode::Enter);
    let selected = app.browser.as_ref().unwrap().selected;
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::Down);
    assert!(app.transfer.is_none());
    assert!(app.deleting.is_none());
    assert_eq!(app.browser.as_ref().unwrap().selected, selected);
    key(&mut app, KeyCode::F(4));
    assert!(app.operation.as_ref().unwrap().cancelling);
    assert!(render(&mut app, 120, 42).contains("Cancelling"));
    key(&mut app, KeyCode::F(3));
    assert!(app.modal.is_none());
    key(&mut app, KeyCode::F(3));
    assert!(matches!(app.modal, Some(Modal::Progress)));
    wait(&mut app);
    assert!(app.operation.is_none());
}
