use crate::scan::{Kind, NodeId, Snapshot};
use ratatui::layout::{Position, Rect};

#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub id: NodeId,
    pub rect: Rect,
}

#[derive(Default, Debug)]
pub struct Treemap {
    pub leaves: Vec<Tile>,
    pub bounds: Vec<Tile>,
}

#[derive(Clone, Copy, Debug)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Treemap {
    /// Hierarchical, weighted binary partition. Terminal cells are about twice as
    /// tall as they are wide, so split direction uses physical aspect ratio.
    /// Zero-cell regions are omitted rather than exaggerating tiny files.
    pub fn build(data: &Snapshot, root: NodeId, area: Rect) -> Self {
        let mut map = Self::default();
        if data.nodes[root].size == 0 || area.is_empty() {
            return map;
        }
        let mut pending = vec![Tile {
            id: root,
            rect: area,
        }];
        while let Some(tile) = pending.pop() {
            map.bounds.push(tile);
            let node = &data.nodes[tile.id];
            if node.kind != Kind::Directory {
                map.leaves.push(tile);
                continue;
            }
            let mut children: Vec<_> = node
                .children
                .iter()
                .copied()
                .filter(|&id| data.nodes[id].size > 0)
                .collect();
            children.sort_by(|&a, &b| {
                data.nodes[b]
                    .size
                    .cmp(&data.nodes[a].size)
                    .then_with(|| data.nodes[a].name.cmp(&data.nodes[b].name))
            });
            let mut partitions = vec![(0, children.len(), tile.rect)];
            while let Some((start, end, rect)) = partitions.pop() {
                if start == end || rect.is_empty() {
                    continue;
                }
                if end - start == 1 {
                    pending.push(Tile {
                        id: children[start],
                        rect,
                    });
                    continue;
                }
                let total: u128 = children[start..end]
                    .iter()
                    .map(|&id| data.nodes[id].size as u128)
                    .sum();
                let mut left = data.nodes[children[start]].size as u128;
                let mut mid = start + 1;
                while mid < end - 1 {
                    let next = left + data.nodes[children[mid]].size as u128;
                    if next.abs_diff(total / 2) >= left.abs_diff(total / 2) {
                        break;
                    }
                    left = next;
                    mid += 1;
                }
                let horizontal = u32::from(rect.width) >= u32::from(rect.height) * 2;
                let length = if horizontal { rect.width } else { rect.height };
                let cut = ((u128::from(length) * left + total / 2) / total) as u16;
                let (a, b) = if horizontal {
                    (
                        Rect::new(rect.x, rect.y, cut, rect.height),
                        Rect::new(rect.x + cut, rect.y, rect.width - cut, rect.height),
                    )
                } else {
                    (
                        Rect::new(rect.x, rect.y, rect.width, cut),
                        Rect::new(rect.x, rect.y + cut, rect.width, rect.height - cut),
                    )
                };
                partitions.push((mid, end, b));
                partitions.push((start, mid, a));
            }
        }
        map
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<NodeId> {
        self.leaves
            .iter()
            .find(|tile| tile.rect.contains(Position::new(x, y)))
            .map(|t| t.id)
    }

    pub fn rect(&self, id: NodeId) -> Option<Rect> {
        self.bounds
            .iter()
            .find(|tile| tile.id == id)
            .map(|tile| tile.rect)
    }

    /// Choose a visible block in the requested half-plane. Prefer blocks aligned
    /// with the current one, then the nearest edge and center.
    pub fn neighbor(&self, selected: NodeId, direction: Direction) -> Option<NodeId> {
        let Some(origin) = self.rect(selected) else {
            return self.leaves.first().map(|t| t.id);
        };
        let cx = i64::from(origin.x) * 2 + i64::from(origin.width);
        let cy = i64::from(origin.y) * 2 + i64::from(origin.height);
        self.leaves
            .iter()
            .filter(|tile| tile.id != selected)
            .filter_map(|tile| {
                let rect = tile.rect;
                let dx = i64::from(rect.x) * 2 + i64::from(rect.width) - cx;
                let dy = i64::from(rect.y) * 2 + i64::from(rect.height) - cy;
                let (forward, cross, overlap) = match direction {
                    Direction::Left => (
                        -dx,
                        dy * 2,
                        rect.y < origin.bottom() && rect.bottom() > origin.y,
                    ),
                    Direction::Right => (
                        dx,
                        dy * 2,
                        rect.y < origin.bottom() && rect.bottom() > origin.y,
                    ),
                    Direction::Up => (
                        -dy * 2,
                        dx,
                        rect.x < origin.right() && rect.right() > origin.x,
                    ),
                    Direction::Down => (
                        dy * 2,
                        dx,
                        rect.x < origin.right() && rect.right() > origin.x,
                    ),
                };
                (forward > 0).then_some((
                    (u8::from(!overlap), forward * forward + cross * cross),
                    tile.id,
                ))
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, id)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Options, scan};
    use std::{fs, sync::atomic::AtomicBool};

    #[test]
    fn tiles_cover_every_cell_without_overlaps_and_hits_agree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        for i in 1..30 {
            fs::write(dir.path().join(format!("sub/{i}")), vec![0; i * 17]).unwrap();
        }
        let data = scan(
            dir.path(),
            Options {
                apparent_size: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        for width in [1, 2, 19, 120] {
            for height in [1, 3, 25] {
                let area = Rect::new(3, 4, width, height);
                let map = Treemap::build(&data, 0, area);
                for y in area.y..area.bottom() {
                    for x in area.x..area.right() {
                        let tiles: Vec<_> = map
                            .leaves
                            .iter()
                            .filter(|t| t.rect.contains(Position::new(x, y)))
                            .collect();
                        assert_eq!(tiles.len(), 1, "{width}x{height} at {x},{y}");
                        assert_eq!(map.hit(x, y), Some(tiles[0].id));
                    }
                }
                assert_eq!(map.hit(0, 0), None);
            }
        }
    }

    #[test]
    fn spatial_navigation_uses_geometry() {
        let map = Treemap {
            leaves: vec![
                Tile {
                    id: 1,
                    rect: Rect::new(0, 0, 10, 5),
                },
                Tile {
                    id: 2,
                    rect: Rect::new(10, 0, 10, 5),
                },
                Tile {
                    id: 3,
                    rect: Rect::new(0, 5, 10, 5),
                },
            ],
            bounds: vec![Tile {
                id: 1,
                rect: Rect::new(0, 0, 10, 5),
            }],
        };
        assert_eq!(map.neighbor(1, Direction::Right), Some(2));
        assert_eq!(map.neighbor(1, Direction::Down), Some(3));
        assert_eq!(map.neighbor(1, Direction::Left), None);
    }
}
