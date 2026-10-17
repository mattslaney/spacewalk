use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use spacewalk::{
    app::{App, Modal},
    browser::Panel,
    git::{RepoState, RepoStatus},
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
    while app.scanning.is_some() || app.deleting.is_some() {
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
    let mut repeat = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    app.key(repeat);
    assert!(app.copying.is_none());
    app.browser.as_mut().unwrap().filter("no-match".into());
    key(&mut app, KeyCode::Char('c'));
    assert!(app.copying.is_none());
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('/'));
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(app.browser.as_ref().unwrap().query, "c");
    assert!(app.copying.is_none());
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('?'));
    key(&mut app, KeyCode::Char('c'));
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
    key(&mut app, KeyCode::Char('v'));
    let screen = render(&mut app, 120, 42);
    assert!(screen.contains("[hidden] photos/"));
    assert!(screen.contains("3.9 KiB shown"));
    assert!(screen.contains("1 hidden [V show all]"));
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
    key(&mut app, KeyCode::Char('v'));
    assert!(!app.browser.as_ref().unwrap().is_hidden(photos));
    key(&mut app, KeyCode::Char('v'));
    key(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 60, 20);
    assert!(screen.contains("No visible space"));
    key(&mut app, KeyCode::Char('V'));
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
    key(&mut app, KeyCode::Char('v'));
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
    key(&mut app, KeyCode::Char('V'));
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
