use crate::{
    scan::{Kind, NodeId, Snapshot},
    treemap::{Direction, Treemap},
};
use ratatui::layout::Rect;
use std::collections::HashSet;

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
    pub focus: Panel,
    pub sort: Sort,
    pub query: String,
    pub matches: Vec<NodeId>,
    pub rows: Vec<TreeRow>,
    pub scroll: usize,
    pub map: Treemap,
    map_area: Rect,
    included: Vec<bool>,
}

impl Browser {
    pub fn new(data: Snapshot) -> Self {
        let included = vec![true; data.nodes.len()];
        let mut browser = Self {
            data,
            selected: 0,
            scope: 0,
            expanded: HashSet::from([0]),
            focus: Panel::Tree,
            sort: Sort::Largest,
            query: String::new(),
            matches: Vec::new(),
            rows: Vec::new(),
            scroll: 0,
            map: Treemap::default(),
            map_area: Rect::default(),
            included,
        };
        browser.sort_nodes();
        browser.rebuild_rows();
        browser
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.sort_nodes();
        self.rebuild_rows();
    }

    fn sort_nodes(&mut self) {
        for id in 0..self.data.nodes.len() {
            let mut children = std::mem::take(&mut self.data.nodes[id].children);
            children.sort_by(|&a, &b| {
                let (a, b) = (&self.data.nodes[a], &self.data.nodes[b]);
                let primary = match self.sort {
                    Sort::Largest => b.size.cmp(&a.size),
                    Sort::Smallest => a.size.cmp(&b.size),
                    Sort::Name => a.name.cmp(&b.name),
                    Sort::Files => b.files.cmp(&a.files),
                };
                primary.then_with(|| a.name.cmp(&b.name))
            });
            self.data.nodes[id].children = children;
        }
    }

    pub fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut pending = vec![(self.scope, 0)];
        while let Some((id, depth)) = pending.pop() {
            if !self.query.is_empty() && !self.included[id] {
                continue;
            }
            self.rows.push(TreeRow { id, depth });
            if !self.query.is_empty() || self.expanded.contains(&id) {
                pending.extend(
                    self.data.nodes[id]
                        .children
                        .iter()
                        .rev()
                        .map(|&child| (child, depth + 1)),
                );
            }
        }
        if !self.rows.iter().any(|row| row.id == self.selected) {
            self.selected = self.rows.first().map_or(self.scope, |row| row.id);
        }
    }

    pub fn filter(&mut self, query: String) {
        self.query = query;
        self.matches.clear();
        self.included.fill(self.query.is_empty());
        if !self.query.is_empty() {
            let needle = self.query.to_lowercase();
            for (id, node) in self.data.nodes.iter().enumerate() {
                if node.name.to_string_lossy().to_lowercase().contains(&needle) {
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
            self.selected = id;
        }
        self.scroll = 0;
    }

    pub fn next_match(&mut self, backwards: bool) {
        let matches: Vec<_> = self
            .rows
            .iter()
            .filter(|r| self.matches.contains(&r.id))
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
        if !self.query.is_empty() && !self.included[id] {
            self.filter(String::new());
        }
        if !self.data.is_within(id, self.scope) {
            self.scope = 0;
            self.map_area = Rect::default();
        }
        let mut current = id;
        while let Some(parent) = self.data.nodes[current].parent {
            self.expanded.insert(parent);
            current = parent;
        }
        self.selected = id;
        self.rebuild_rows();
    }

    pub fn move_tree(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let index = self
            .rows
            .iter()
            .position(|r| r.id == self.selected)
            .unwrap_or(0);
        let next = index.saturating_add_signed(delta).min(self.rows.len() - 1);
        self.selected = self.rows[next].id;
    }

    pub fn expand(&mut self) {
        if self.data.nodes[self.selected].kind != Kind::Directory {
            return;
        }
        if self.expanded.insert(self.selected) {
            self.rebuild_rows();
        } else {
            self.move_tree(1);
        }
    }

    pub fn collapse(&mut self) {
        if self.expanded.remove(&self.selected) {
            self.rebuild_rows();
        } else if self.selected != self.scope {
            if let Some(parent) = self.data.nodes[self.selected].parent {
                self.selected = parent;
            }
        }
    }

    pub fn toggle(&mut self, id: NodeId) {
        if self.data.nodes[id].kind != Kind::Directory {
            return;
        }
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild_rows();
    }

    pub fn zoom_in(&mut self) {
        if self.data.nodes[self.selected].kind != Kind::Directory {
            return;
        }
        self.scope = self.selected;
        self.expanded.insert(self.scope);
        self.scroll = 0;
        self.rebuild_rows();
        self.map_area = Rect::default();
    }

    pub fn zoom_out(&mut self) {
        if let Some(parent) = self.data.nodes[self.scope].parent {
            let old = self.scope;
            self.scope = parent;
            self.expanded.insert(parent);
            self.select(old);
            self.map_area = Rect::default();
        }
    }

    pub fn layout_map(&mut self, area: Rect) {
        if self.map_area != area {
            self.map = Treemap::build(&self.data, self.scope, area);
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
        if let Some(index) = self.rows.iter().position(|r| r.id == self.selected) {
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
}
