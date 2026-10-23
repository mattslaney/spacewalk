use crate::{
    filters::ViewFilters,
    git::{self, RepoState},
    scan::{Kind, NodeId, Snapshot},
    treemap::{Direction, Treemap},
};
use ratatui::layout::Rect;
use std::{
    collections::{HashMap, HashSet},
    time::SystemTime,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Panel {
    #[default]
    Tree,
    Map,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum Sort {
    #[default]
    Largest,
    Name,
    Files,
    Smallest,
}

impl Sort {
    pub fn next(self) -> Self {
        match self {
            Self::Largest => Self::Name,
            Self::Name => Self::Files,
            Self::Files => Self::Smallest,
            Self::Smallest => Self::Largest,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Largest => "size ↓",
            Self::Name => "name ↑",
            Self::Files => "files ↓",
            Self::Smallest => "size ↑",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TreeRow {
    pub id: NodeId,
    pub depth: usize,
}

pub struct Browser {
    pub data: Snapshot,
    pub selected: NodeId,
    pub scope: NodeId,
    pub expanded: HashSet<NodeId>,
    // Explicit folds override paths that search or view filters open by default.
    pub collapsed: HashSet<NodeId>,
    pub focus: Panel,
    pub sort: Sort,
    pub query: String,
    pub matches: Vec<NodeId>,
    pub rows: Vec<TreeRow>,
    pub scroll: usize,
    pub map: Treemap,
    pub repositories: HashMap<NodeId, RepoState>,
    pub marked: HashSet<NodeId>,
    pub view_filters: ViewFilters,
    view_passed: Vec<bool>,
    view_included: Vec<bool>,
    filtered_sizes: Vec<u64>,
    filtered_files: Vec<u64>,
    map_area: Rect,
    // Node IDs map to visible row offsets; usize::MAX marks a hidden row.
    row_positions: Vec<usize>,
    included: Vec<bool>,
    hidden: HashSet<NodeId>,
    hidden_effective: Vec<bool>,
    map_sizes: Vec<u64>,
}

impl Browser {
    pub fn new(data: Snapshot) -> Self {
        let included = vec![true; data.nodes.len()];
        let row_positions = vec![usize::MAX; data.nodes.len()];
        let repositories = git::discover(&data);
        let mut browser = Self {
            data,
            selected: 0,
            scope: 0,
            expanded: HashSet::from([0]),
            collapsed: HashSet::new(),
            focus: Panel::Tree,
            sort: Sort::Largest,
            query: String::new(),
            matches: Vec::new(),
            rows: Vec::new(),
            scroll: 0,
            map: Treemap::default(),
            repositories,
            marked: HashSet::new(),
            view_filters: ViewFilters::default(),
            view_passed: Vec::new(),
            view_included: Vec::new(),
            filtered_sizes: Vec::new(),
            filtered_files: Vec::new(),
            map_area: Rect::default(),
            row_positions,
            included,
            hidden: HashSet::new(),
            hidden_effective: Vec::new(),
            map_sizes: Vec::new(),
        };
        browser.update_map_sizes();
        browser.sort_nodes();
        browser.rebuild_rows();
        browser
    }

    pub fn set_view_filters(&mut self, filters: ViewFilters) {
        self.set_view_filters_at(filters, SystemTime::now());
    }

    pub fn set_view_filters_at(&mut self, filters: ViewFilters, now: SystemTime) {
        self.view_filters = filters;
        self.view_passed.clear();
        self.view_included.clear();
        self.filtered_sizes.clear();
        self.filtered_files.clear();
        if filters.active() {
            let cutoff = filters.cutoff(now);
            for (id, node) in self.data.nodes.iter().enumerate() {
                let inherited = node.parent.is_some_and(|p| self.view_passed[p]);
                let passes = id != 0
                    && filters.min_size.is_none_or(|min| node.size >= min)
                    && (filters.min_age_days.is_none()
                        || node
                            .modified
                            .zip(cutoff)
                            .is_some_and(|(modified, cutoff)| modified <= cutoff));
                let passed = inherited || passes;
                self.view_passed.push(passed);
                self.view_included.push(passed);
                self.filtered_sizes
                    .push(if passed && node.kind != Kind::Directory {
                        node.size
                    } else {
                        0
                    });
                self.filtered_files
                    .push(u64::from(passed && node.kind != Kind::Directory));
            }
            for id in (1..self.data.nodes.len()).rev() {
                let parent = self.data.nodes[id].parent.unwrap();
                self.view_included[parent] |= self.view_included[id];
                self.filtered_sizes[parent] =
                    self.filtered_sizes[parent].saturating_add(self.filtered_sizes[id]);
                self.filtered_files[parent] =
                    self.filtered_files[parent].saturating_add(self.filtered_files[id]);
            }
        }
        self.update_map_sizes();
        self.sort_nodes();
        self.filter(self.query.clone());
        self.scroll = 0;
    }
    pub fn in_view(&self, id: NodeId) -> bool {
        !self.view_filters.active() || self.view_included[id]
    }
    pub fn fully_in_view(&self, id: NodeId) -> bool {
        !self.view_filters.active() || self.view_passed[id]
    }
    pub fn is_context(&self, id: NodeId) -> bool {
        self.in_view(id) && !self.fully_in_view(id)
    }
    pub fn filtered_size(&self, id: NodeId) -> u64 {
        if self.view_filters.active() {
            self.filtered_sizes[id]
        } else {
            self.data.nodes[id].size
        }
    }
    pub fn filtered_file_count(&self, id: NodeId) -> u64 {
        if self.view_filters.active() {
            self.filtered_files[id]
        } else {
            self.data.nodes[id].files
        }
    }
    pub fn open_in_view(&self, id: NodeId) -> bool {
        !self.collapsed.contains(&id)
            && (self.expanded.contains(&id) || !self.query.is_empty() || self.is_context(id))
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.sort_nodes();
        self.rebuild_rows();
    }

    pub fn toggle_mark(&mut self, id: NodeId) {
        if id == 0 {
            return;
        }
        if !self.marked.remove(&id) {
            self.marked.insert(id);
        }
    }

    pub fn hidden_items(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.hidden.iter().copied()
    }

    pub fn set_hidden(&mut self, items: impl IntoIterator<Item = NodeId>) {
        self.hidden = items.into_iter().collect();
        self.update_map_sizes();
    }

    pub fn is_hidden(&self, id: NodeId) -> bool {
        self.hidden_effective[id]
    }

    pub fn visible_size(&self, id: NodeId) -> u64 {
        self.map_sizes[id]
    }

    pub fn hidden_in_scope(&self) -> usize {
        self.hidden
            .iter()
            .filter(|&&id| {
                self.data.is_within(id, self.scope) || self.data.is_within(self.scope, id)
            })
            .count()
    }

    pub fn toggle_visibility(&mut self, id: NodeId) {
        if self.is_hidden(id) {
            // Showing a child also shows its ancestors; independently hidden
            // siblings retain their own visibility choices.
            let mut current = Some(id);
            while let Some(id) = current {
                self.hidden.remove(&id);
                current = self.data.nodes[id].parent;
            }
        } else {
            self.hidden.insert(id);
        }
        self.update_map_sizes();
    }

    pub fn show_all(&mut self) {
        self.hidden.clear();
        self.update_map_sizes();
    }

    fn update_map_sizes(&mut self) {
        self.hidden_effective.clear();
        self.map_sizes.clear();
        for (id, node) in self.data.nodes.iter().enumerate() {
            let hidden = self.hidden.contains(&id)
                || node
                    .parent
                    .is_some_and(|parent| self.hidden_effective[parent]);
            self.hidden_effective.push(hidden);
            self.map_sizes.push(
                if hidden || node.kind == Kind::Directory || !self.fully_in_view(id) {
                    0
                } else {
                    node.size
                },
            );
        }
        // Children follow parents in the arena, so accumulate visible bytes in
        // reverse order without changing the scan's actual disk usage totals.
        for id in (1..self.data.nodes.len()).rev() {
            let parent = self.data.nodes[id].parent.unwrap();
            self.map_sizes[parent] = self.map_sizes[parent].saturating_add(self.map_sizes[id]);
        }
        self.map_area = Rect::default();
    }

    fn sort_nodes(&mut self) {
        for id in 0..self.data.nodes.len() {
            let mut children = std::mem::take(&mut self.data.nodes[id].children);
            children.sort_by(|&a, &b| {
                let (a_size, b_size) = (self.filtered_size(a), self.filtered_size(b));
                let (a_files, b_files) = (self.filtered_file_count(a), self.filtered_file_count(b));
                let (a, b) = (&self.data.nodes[a], &self.data.nodes[b]);
                let primary = match self.sort {
                    Sort::Largest => b_size.cmp(&a_size),
                    Sort::Smallest => a_size.cmp(&b_size),
                    Sort::Name => a.name.cmp(&b.name),
                    Sort::Files => b_files.cmp(&a_files),
                };
                primary.then_with(|| a.name.cmp(&b.name))
            });
            self.data.nodes[id].children = children;
        }
    }

    pub fn rebuild_rows(&mut self) {
        for row in &self.rows {
            self.row_positions[row.id] = usize::MAX;
        }
        self.rows.clear();
        let mut pending = vec![(self.scope, 0)];
        while let Some((id, depth)) = pending.pop() {
            if !self.in_view(id) || (!self.query.is_empty() && !self.included[id]) {
                continue;
            }
            self.row_positions[id] = self.rows.len();
            self.rows.push(TreeRow { id, depth });
            if self.open_in_view(id) {
                pending.extend(
                    self.data.nodes[id]
                        .children
                        .iter()
                        .rev()
                        .map(|&child| (child, depth + 1)),
                );
            }
        }
        if self.row_position(self.selected).is_none() {
            self.selected = self.rows.first().map_or(self.scope, |row| row.id);
        }
    }

    fn row_position(&self, id: NodeId) -> Option<usize> {
        let index = self.row_positions[id];
        (index != usize::MAX).then_some(index)
    }

    pub fn filter(&mut self, query: String) {
        self.query = query;
        if self.query.is_empty() {
            let mut current = self.selected;
            while let Some(parent) = self.data.nodes[current].parent {
                self.expanded.insert(parent);
                self.collapsed.remove(&parent);
                current = parent;
            }
        }
        self.matches.clear();
        self.included.fill(self.query.is_empty());
        if !self.query.is_empty() {
            let needle = self.query.to_lowercase();
            for (id, node) in self.data.nodes.iter().enumerate() {
                if self.in_view(id) && node.name.to_string_lossy().to_lowercase().contains(&needle)
                {
                    self.included[id] = true;
                    self.matches.push(id);
                }
            }
            for id in (1..self.data.nodes.len()).rev() {
                if self.included[id] {
                    self.included[self.data.nodes[id].parent.unwrap()] = true;
                }
            }
        }
        self.rebuild_rows();
        if let Some(id) = self
            .matches
            .iter()
            .copied()
            .find(|&id| self.data.is_within(id, self.scope))
        {
            self.select(id);
        }
        self.scroll = 0;
    }

    pub fn next_match(&mut self, backwards: bool) {
        let matches: Vec<_> = self
            .rows
            .iter()
            .filter(|r| self.matches.binary_search(&r.id).is_ok())
            .map(|r| r.id)
            .collect();
        if matches.is_empty() {
            return;
        }
        let index = matches.iter().position(|&id| id == self.selected);
        let next = match (index, backwards) {
            (Some(i), true) => (i + matches.len() - 1) % matches.len(),
            (Some(i), false) => (i + 1) % matches.len(),
            (None, true) => matches.len() - 1,
            (None, false) => 0,
        };
        self.selected = matches[next];
    }

    /// One selection is shared by both panels. Reveal map-picked files by
    /// expanding every ancestor; clear a filter if it would hide the selection.
    pub fn select(&mut self, id: NodeId) {
        if !self.in_view(id) {
            return;
        }
        if !self.query.is_empty() && !self.included[id] {
            self.filter(String::new());
        }
        let scope_changed = !self.data.is_within(id, self.scope);
        if scope_changed {
            self.scope = 0;
            self.map_area = Rect::default();
        }
        let mut current = id;
        while let Some(parent) = self.data.nodes[current].parent {
            self.expanded.insert(parent);
            self.collapsed.remove(&parent);
            current = parent;
        }
        self.selected = id;
        // Already-visible selections only change the highlight. Rebuild when
        // revealing a collapsed path or returning to the root scope.
        if scope_changed || self.row_position(id).is_none() {
            self.rebuild_rows();
        }
    }

    pub fn move_tree(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let index = self.row_position(self.selected).unwrap_or(0);
        let next = index.saturating_add_signed(delta).min(self.rows.len() - 1);
        self.selected = self.rows[next].id;
    }

    pub fn expand(&mut self) {
        if self.data.nodes[self.selected].kind != Kind::Directory {
            return;
        }
        let folded = self.collapsed.remove(&self.selected);
        if self.expanded.insert(self.selected) || folded {
            self.rebuild_rows();
        } else if let Some(row) = self
            .rows
            .iter()
            .find(|row| self.data.nodes[row.id].parent == Some(self.selected))
        {
            self.selected = row.id;
        }
    }

    pub fn collapse(&mut self) {
        if self.data.nodes[self.selected].kind == Kind::Directory
            && self.open_in_view(self.selected)
        {
            self.expanded.remove(&self.selected);
            self.collapsed.insert(self.selected);
            self.rebuild_rows();
        } else if self.selected != self.scope
            && let Some(parent) = self.data.nodes[self.selected].parent
        {
            self.selected = parent;
        }
    }

    pub fn toggle(&mut self, id: NodeId) {
        if self.data.nodes[id].kind != Kind::Directory {
            return;
        }
        if self.open_in_view(id) {
            self.expanded.remove(&id);
            self.collapsed.insert(id);
        } else {
            self.collapsed.remove(&id);
            self.expanded.insert(id);
        }
        self.rebuild_rows();
    }

    pub fn expand_all(&mut self) {
        self.set_subtree_expanded(true);
    }
    pub fn collapse_all(&mut self) {
        self.set_subtree_expanded(false);
    }

    fn set_subtree_expanded(&mut self, expanded: bool) {
        if self.rows.is_empty() || self.data.nodes[self.selected].kind != Kind::Directory {
            return;
        }
        let mut pending = vec![self.selected];
        while let Some(id) = pending.pop() {
            if self.data.nodes[id].kind != Kind::Directory {
                continue;
            }
            if expanded {
                self.collapsed.remove(&id);
                self.expanded.insert(id);
            } else {
                self.expanded.remove(&id);
                self.collapsed.insert(id);
            }
            pending.extend(self.data.nodes[id].children.iter().copied());
        }
        self.rebuild_rows();
    }

    pub fn zoom_in(&mut self) {
        if self.data.nodes[self.selected].kind != Kind::Directory {
            return;
        }
        self.scope = self.selected;
        self.expanded.insert(self.scope);
        self.collapsed.remove(&self.scope);
        self.scroll = 0;
        self.rebuild_rows();
        self.map_area = Rect::default();
    }

    pub fn zoom_out(&mut self) {
        if let Some(parent) = self.data.nodes[self.scope].parent {
            let old = self.scope;
            self.scope = parent;
            self.expanded.insert(parent);
            self.collapsed.remove(&parent);
            self.select(old);
            self.rebuild_rows();
            self.map_area = Rect::default();
        }
    }

    pub fn layout_map(&mut self, area: Rect) {
        if self.map_area != area {
            self.map = Treemap::build_with_sizes(&self.data, self.scope, area, &self.map_sizes);
            self.map_area = area;
        }
    }

    pub fn move_map(&mut self, direction: Direction) {
        if let Some(id) = self.map.neighbor(self.selected, direction) {
            self.select(id);
        }
    }

    pub fn ensure_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if let Some(index) = self.row_position(self.selected) {
            if index < self.scroll {
                self.scroll = index;
            }
            if index >= self.scroll + height {
                self.scroll = index + 1 - height;
            }
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(height));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Options, scan};
    use std::{fs, sync::atomic::AtomicBool};

    fn fixture() -> (tempfile::TempDir, Browser) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
        fs::write(dir.path().join("sub/deep/big.bin"), [0; 900]).unwrap();
        fs::write(dir.path().join("small.txt"), [0; 100]).unwrap();
        let data = scan(
            dir.path(),
            Options {
                apparent_size: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        (dir, Browser::new(data))
    }
    fn id(b: &Browser, name: &str) -> NodeId {
        b.data.nodes.iter().position(|n| n.name == name).unwrap()
    }

    #[test]
    fn map_selection_reveals_tree_and_tree_selection_has_matching_bounds() {
        let (_dir, mut b) = fixture();
        b.layout_map(Rect::new(0, 0, 80, 20));
        let big = id(&b, "big.bin");
        b.select(big);
        assert!(b.rows.iter().any(|r| r.id == big));
        assert!(b.expanded.contains(&id(&b, "deep")));
        let rect = b.map.rect(big).unwrap();
        assert_eq!(b.map.hit(rect.x, rect.y), Some(b.selected));
        b.move_tree(-1);
        assert!(b.map.rect(b.selected).is_some());
    }

    #[test]
    fn filter_keeps_ancestors_and_map_pick_clears_hiding_filter() {
        let (_dir, mut b) = fixture();
        b.filter("BIG".into());
        assert_eq!(b.rows.len(), 4);
        assert_eq!(b.selected, id(&b, "big.bin"));
        b.select(id(&b, "small.txt"));
        assert!(b.query.is_empty());
        assert_eq!(b.selected, id(&b, "small.txt"));
        b.filter("no-match".into());
        assert!(b.rows.is_empty());
    }

    #[test]
    fn zoom_sort_and_collapse_preserve_coherent_selection() {
        let (_dir, mut b) = fixture();
        assert_eq!(b.rows[1].id, id(&b, "sub"));
        b.cycle_sort();
        assert_eq!(b.rows[1].id, id(&b, "small.txt"));
        b.select(id(&b, "sub"));
        b.zoom_in();
        assert_eq!(b.scope, id(&b, "sub"));
        b.zoom_out();
        assert_eq!(b.scope, 0);
        assert_eq!(b.selected, id(&b, "sub"));
        b.select(id(&b, "big.bin"));
        b.collapse();
        assert_eq!(b.selected, id(&b, "deep"));
    }

    #[test]
    fn navigation_follows_current_rows_after_view_changes() {
        fn walk_rows(b: &mut Browser) {
            let ids: Vec<_> = b.rows.iter().map(|row| row.id).collect();
            b.selected = ids[0];
            for &id in &ids {
                assert_eq!(b.selected, id);
                b.ensure_visible(1);
                assert_eq!(b.rows[b.scroll].id, id);
                b.move_tree(1);
            }
            for &id in ids.iter().rev() {
                assert_eq!(b.selected, id);
                b.ensure_visible(1);
                assert_eq!(b.rows[b.scroll].id, id);
                b.move_tree(-1);
            }
        }

        let (_dir, mut b) = fixture();
        let big = id(&b, "big.bin");
        let deep = id(&b, "deep");
        let sub = id(&b, "sub");
        let small = id(&b, "small.txt");
        b.select(big);
        walk_rows(&mut b);
        b.select(deep);
        b.collapse();
        assert!(!b.rows.iter().any(|row| row.id == big));
        b.select(big);
        assert!(b.rows.iter().any(|row| row.id == big));
        walk_rows(&mut b);
        b.cycle_sort();
        walk_rows(&mut b);
        b.filter("big".into());
        walk_rows(&mut b);
        b.select(small);
        assert!(b.query.is_empty());
        walk_rows(&mut b);
        b.select(deep);
        b.zoom_in();
        walk_rows(&mut b);
        b.zoom_out();
        assert_eq!(b.rows[0].id, sub);
        walk_rows(&mut b);
        b.select(small);
        assert_eq!(b.rows[0].id, 0);
        walk_rows(&mut b);
        b.filter("no-match".into());
        b.move_tree(1);
        b.ensure_visible(1);
        assert!(b.rows.is_empty());
        b.filter(String::new());
        walk_rows(&mut b);
    }

    #[test]
    fn hiding_a_subtree_reallocates_the_map_without_changing_disk_totals() {
        let (_dir, mut b) = fixture();
        let area = Rect::new(3, 4, 80, 20);
        let sub = id(&b, "sub");
        let big = id(&b, "big.bin");
        let small = id(&b, "small.txt");
        b.select(sub);
        b.layout_map(area);
        b.toggle_visibility(sub);
        b.layout_map(area);
        assert_eq!(b.data.nodes[0].size, 1000);
        assert_eq!(b.visible_size(0), 100);
        assert!(b.is_hidden(sub) && b.is_hidden(big));
        assert_eq!(b.map.leaves.len(), 1);
        assert_eq!(b.map.leaves[0].id, small);
        assert_eq!(b.map.leaves[0].rect, area);
        assert_eq!(b.map.hit(area.x, area.y), Some(small));
        assert!(b.map.rect(sub).is_none());
        assert_eq!(b.selected, sub);
        assert!(b.rows.iter().any(|row| row.id == sub));
        b.zoom_in();
        b.layout_map(area);
        assert!(b.map.leaves.is_empty());
        assert_eq!(b.hidden_in_scope(), 1);
        b.zoom_out();
        b.toggle_visibility(sub);
        b.layout_map(area);
        assert_eq!(b.visible_size(0), 1000);
        assert_eq!(b.map.leaves.len(), 2);
    }

    #[test]
    fn showing_a_child_reveals_ancestors_and_keeps_other_hidden_items() {
        let (_dir, mut b) = fixture();
        let big = id(&b, "big.bin");
        let small = id(&b, "small.txt");
        b.set_hidden([id(&b, "sub"), big, small]);
        assert_eq!(b.visible_size(0), 0);
        b.toggle_visibility(big);
        assert!(!b.is_hidden(big));
        assert!(!b.is_hidden(id(&b, "sub")));
        assert!(b.is_hidden(small));
        assert_eq!(b.hidden_items().collect::<Vec<_>>(), vec![small]);
        assert_eq!(b.visible_size(0), 900);
        b.toggle_visibility(0);
        assert_eq!(b.visible_size(0), 0);
        b.show_all();
        assert_eq!(b.hidden_items().count(), 0);
        assert_eq!(b.visible_size(0), 1000);
    }
}
