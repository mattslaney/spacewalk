use ratatui::layout::Rect;
use spacewalk::{
    browser::Browser,
    filters::ViewFilters,
    scan::{self, Options},
};
use std::{
    fs::{self, File, FileTimes},
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime},
};
fn modified(path: &Path, time: SystemTime) {
    File::open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(time))
        .unwrap();
}
fn browser(path: &Path) -> Browser {
    Browser::new(
        scan::scan(
            path,
            Options {
                apparent_size: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap(),
    )
}
fn id(b: &Browser, path: &Path) -> usize {
    b.data.find_path(path).unwrap()
}
fn filters(size: u64, days: u64) -> ViewFilters {
    ViewFilters {
        min_size: (size != 0).then_some(size),
        min_age_days: (days != 0).then_some(days),
    }
}

#[test]
fn age_and_size_hide_excluded_items_from_tree_and_map_and_clear_restores_them() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let old = now - Duration::from_secs(200 * 86400);
    for (name, size, time) in [
        ("old-large", 1000, old),
        ("new-large", 900, now),
        ("old-small", 10, old),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, vec![0; size]).unwrap();
        modified(&path, time);
    }
    let mut b = browser(dir.path());
    let old_id = id(&b, &dir.path().join("old-large"));
    let total = b.data.nodes[0].size;
    b.set_view_filters_at(filters(500, 90), now);
    b.layout_map(Rect::new(0, 0, 100, 30));
    assert_eq!(
        b.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![0, old_id]
    );
    assert_eq!(
        b.map.leaves.iter().map(|tile| tile.id).collect::<Vec<_>>(),
        vec![old_id]
    );
    assert_eq!(b.visible_size(0), 1000);
    assert_eq!(b.filtered_file_count(0), 1);
    assert_eq!(b.data.nodes[0].size, total);
    let excluded = id(&b, &dir.path().join("new-large"));
    b.select(excluded);
    assert_ne!(b.selected, excluded);
    assert!(b.view_filters.active());
    b.set_view_filters(ViewFilters::default());
    b.layout_map(Rect::new(0, 0, 100, 30));
    assert!(b.map.leaves.iter().any(|tile| tile.id == excluded));
    assert!(b.map.leaves.iter().any(|tile| tile.id == old_id));
    assert_eq!(b.rows.len(), 4);
    assert_eq!(b.visible_size(0), total);
}

#[test]
fn matching_old_large_folders_include_their_small_contents_and_context_paths_expand() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let old = now - Duration::from_secs(200 * 86400);
    let folder = dir.path().join("context/build");
    fs::create_dir_all(&folder).unwrap();
    for name in ["one", "two"] {
        let path = folder.join(name);
        fs::write(&path, vec![0; 600]).unwrap();
        modified(&path, old);
    }
    modified(&folder, old);
    let mut b = browser(dir.path());
    let context = id(&b, &dir.path().join("context"));
    let build = id(&b, &folder);
    assert_eq!(b.data.nodes[build].modified, Some(old));
    assert!(b.data.nodes[context].modified.unwrap() > old);
    b.set_view_filters_at(filters(1000, 90), now);
    assert!(b.is_context(context));
    assert!(b.fully_in_view(build));
    assert!(b.rows.iter().any(|r| r.id == build));
    b.select(build);
    b.expand();
    b.layout_map(Rect::new(0, 0, 100, 30));
    assert_eq!(b.map.leaves.len(), 2);
    assert_eq!(b.filtered_size(build), 1200);
    assert!(b.rows.iter().any(|r| b.data.nodes[r.id].name == "one"));
}

#[test]
fn recent_or_unknown_descendants_stop_a_folder_from_matching_age_and_empty_views_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let old = now - Duration::from_secs(200 * 86400);
    let folder = dir.path().join("mixed");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("old"), vec![0; 600]).unwrap();
    modified(&folder.join("old"), old);
    fs::write(folder.join("new"), vec![0; 600]).unwrap();
    modified(&folder, old);
    let mut b = browser(dir.path());
    let mixed = id(&b, &folder);
    assert!(b.data.nodes[mixed].modified.unwrap() > old);
    b.set_view_filters_at(filters(1000, 90), now);
    assert!(b.rows.is_empty());
    b.layout_map(Rect::new(0, 0, 100, 30));
    assert!(b.map.leaves.is_empty());
    b.move_tree(1);
    b.ensure_visible(2);
    b.zoom_out();
    b.set_view_filters_at(filters(0, 90), now);
    assert_eq!(b.filtered_size(0), 600);
    let old_id = id(&b, &folder.join("old"));
    b.data.nodes[old_id].modified = None;
    b.set_view_filters_at(filters(0, 90), now);
    assert!(b.rows.is_empty());
}

#[test]
fn age_size_name_search_manual_hiding_and_zoom_remain_independent() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("big-one"), vec![0; 1000]).unwrap();
    fs::write(dir.path().join("big-two"), vec![0; 900]).unwrap();
    fs::write(dir.path().join("tiny"), vec![0; 5]).unwrap();
    let mut b = browser(dir.path());
    let one = id(&b, &dir.path().join("big-one"));
    let two = id(&b, &dir.path().join("big-two"));
    b.set_view_filters(filters(500, 0));
    b.filter("one".into());
    assert!(b.rows.iter().any(|r| r.id == one));
    assert!(!b.rows.iter().any(|r| r.id == two));
    b.layout_map(Rect::new(0, 0, 100, 30));
    assert_eq!(b.map.leaves.len(), 2);
    b.select(two);
    assert!(b.query.is_empty());
    assert!(b.view_filters.active());
    b.toggle_visibility(one);
    assert_eq!(b.visible_size(0), 900);
    b.show_all();
    assert_eq!(b.visible_size(0), 1900);
    b.set_view_filters(ViewFilters::default());
    assert_eq!(b.visible_size(0), 1905);
}
