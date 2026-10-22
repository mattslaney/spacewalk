use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use spacewalk::{
    app::{App, Modal},
    browser::Panel,
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
    app.poll_workers();
    assert!(app.quitting);
    assert!(app.scanning.is_none());
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
