use crate::{
    app::{App, HitAreas, Modal},
    browser::{Browser, Panel},
    display_name,
    git::RepoState,
    human_size,
    scan::{Kind, Node},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use std::collections::BTreeMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const BG: Color = Color::Rgb(15, 20, 30);
const PANEL: Color = Color::Rgb(20, 27, 39);
const TEXT: Color = Color::Rgb(220, 229, 239);
const MUTED: Color = Color::Rgb(133, 151, 174);
const EDGE: Color = Color::Rgb(55, 72, 92);
const ACCENT: Color = Color::Rgb(107, 226, 188);
const WARN: Color = Color::Rgb(250, 188, 111);
const SELECTED: Color = Color::Rgb(38, 65, 75);
const PALETTE: [Color; 8] = [
    Color::Rgb(80, 173, 209),
    Color::Rgb(153, 127, 218),
    Color::Rgb(87, 185, 148),
    Color::Rgb(218, 149, 95),
    Color::Rgb(210, 111, 151),
    Color::Rgb(116, 146, 219),
    Color::Rgb(189, 183, 102),
    Color::Rgb(80, 182, 184),
];

fn extension(node: &Node) -> String {
    if node.kind == Kind::Symlink {
        return "symlink".into();
    }
    std::path::Path::new(&node.name)
        .extension()
        .map(|ext| format!(".{}", display_name(ext).to_lowercase()))
        .unwrap_or_else(|| "no ext".into())
}

fn color(key: &str) -> Color {
    let hash = key.bytes().fold(2166136261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16777619)
    });
    PALETTE[hash as usize % PALETTE.len()]
}

fn shade(color: Color, factor: f32) -> Color {
    if let Color::Rgb(r, g, b) = color {
        Color::Rgb(
            (f32::from(r) * factor) as u8,
            (f32::from(g) * factor) as u8,
            (f32::from(b) * factor) as u8,
        )
    } else {
        color
    }
}

fn git_color(state: &RepoState) -> Color {
    match state {
        RepoState::Checking => MUTED,
        RepoState::Ready(status) if status.synced() => ACCENT,
        RepoState::Ready(status) if !status.has_remote => Color::Rgb(180, 155, 225),
        _ => WARN,
    }
}

pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let size = ch.width().unwrap_or(0);
        if used + size > width - 1 {
            break;
        }
        result.push(ch);
        used += size;
    }
    result.push('…');
    result
}

fn panel(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { ACCENT } else { EDGE }))
        .style(Style::default().bg(PANEL))
        .title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(if focused { ACCENT } else { MUTED })
                .bold(),
        )))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
    app.hits = HitAreas::default();
    if area.width < 60 || area.height < 20 {
        frame.render_widget(
            Paragraph::new("SPACEWALK\n\nResize to at least 60 × 20.\nq quit · ? help")
                .style(Style::default().fg(ACCENT))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let tree_height = (area.height.saturating_sub(9) * 2 / 5).max(5);
    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(tree_height),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    let state = if app.deleting.is_some() {
        "DELETING"
    } else if app.scanning.is_some() {
        "SCANNING"
    } else {
        "READY"
    };
    let header = Line::from(vec![
        Span::styled(" ◈ SPACEWALK ", Style::default().fg(ACCENT).bold()),
        Span::styled(" / ", Style::default().fg(EDGE)),
        Span::styled(
            truncate(
                &display_name(app.root.as_os_str()),
                area.width.saturating_sub(40) as usize,
            ),
            Style::default().fg(TEXT),
        ),
        Span::styled(
            format!("   {state}"),
            Style::default().fg(if state == "READY" { MUTED } else { WARN }),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), sections[0]);
    if let Some(b) = app.browser.as_mut() {
        let total = &b.data.nodes[0];
        let errors = if b.data.errors > 0 {
            format!("   ⚠ {} errors [e] · totals partial", b.data.errors)
        } else {
            String::new()
        };
        let measure = if b.data.options.apparent_size {
            "apparent"
        } else if cfg!(unix) {
            "allocated"
        } else {
            "file size"
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", human_size(total.size)),
                    Style::default().fg(ACCENT).bold(),
                ),
                Span::styled(
                    format!(
                        "{measure}   {} files   {} dirs   {:.2}s",
                        total.files,
                        b.data.nodes.len() as u64 - total.files - 1,
                        b.data.elapsed.as_secs_f64()
                    ),
                    Style::default().fg(MUTED),
                ),
                Span::styled(errors, Style::default().fg(WARN)),
            ])),
            sections[1],
        );
        draw_tree(frame, b, sections[2], &mut app.hits);
        draw_map(frame, b, sections[3], &mut app.hits);
        draw_legend(frame, b, sections[4]);
        let selected = &b.data.nodes[b.selected];
        let path = display_name(b.data.path(b.selected).as_os_str());
        let note = if b.is_hidden(b.selected) {
            "  hidden from map · h show · H show all".into()
        } else if b.visible_size(b.selected) < selected.size {
            format!(
                "  {} visible in map · H show all",
                human_size(b.visible_size(b.selected))
            )
        } else if let Some(error) = &selected.error {
            format!("  ⚠ {error}")
        } else if selected.hard_link {
            "  hard link · space counted at another path".into()
        } else if selected.kind == Kind::Symlink {
            "  symbolic link · target not followed".into()
        } else if selected.size == 0 {
            "  zero bytes · no visible block".into()
        } else if b.map.rect(b.selected).is_none() {
            "  below map resolution · zoom into its parent".into()
        } else {
            String::new()
        };
        let mut detail = if b.rows.is_empty() {
            vec![
                Line::from(Span::styled(
                    " No matching items",
                    Style::default().fg(WARN),
                )),
                Line::from(" Esc clears the filter"),
            ]
        } else {
            vec![
                Line::from(Span::styled(
                    format!(" {}", truncate(&path, area.width as usize - 2)),
                    Style::default().fg(TEXT),
                )),
                Line::from(vec![
                    Span::styled(
                        format!(" {} · {} files", human_size(selected.size), selected.files),
                        Style::default().fg(ACCENT),
                    ),
                    Span::styled(note, Style::default().fg(MUTED)),
                ]),
            ]
        };
        if !b.rows.is_empty()
            && let Some(repository) = b.repositories.get(&b.selected)
        {
            detail.push(Line::from(Span::styled(
                format!(
                    " {}",
                    truncate(&repository.summary(), area.width.saturating_sub(2) as usize)
                ),
                Style::default().fg(git_color(repository)),
            )));
        }
        frame.render_widget(Paragraph::new(detail), sections[5]);
    } else {
        let text = if app.scanning.is_some() {
            format!(
                "\n  Scanning your files…\n\n  {} entries · {} found · {} errors\n\n  {}\n\n  You can quit with q while the scan is running.",
                app.progress.entries,
                human_size(app.progress.bytes),
                app.progress.errors,
                display_name(app.progress.current.as_os_str())
            )
        } else {
            format!("\n  {}\n\n  r retry · q quit", app.status)
        };
        let body = Rect::new(
            sections[2].x,
            sections[2].y,
            sections[2].width,
            sections[5].bottom() - sections[2].y,
        );
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(MUTED)),
            body,
        );
    }
    if app.searching {
        let query = app.browser.as_ref().map_or("", |b| &b.query);
        frame.render_widget(
            Paragraph::new(format!(" /{query}▏   Enter apply · Esc clear"))
                .style(Style::default().fg(ACCENT).bg(SELECTED)),
            sections[6],
        );
    } else if app.scanning.is_some() && app.browser.is_some() {
        frame.render_widget(
            Paragraph::new(format!(
                " Scanning… {} entries · {} · q quit",
                app.progress.entries,
                human_size(app.progress.bytes)
            ))
            .style(Style::default().fg(WARN)),
            sections[6],
        );
    } else if app.deleting.is_some() {
        frame.render_widget(
            Paragraph::new(app.status.as_str()).style(Style::default().fg(WARN)),
            sections[6],
        );
    } else {
        let marked = app.browser.as_ref().map_or(0, |b| b.marked.len());
        let keys = if marked > 0 {
            format!(" × {marked} marked · m toggle · M clear · d/Delete delete · ? help")
        } else if area.width >= 100 {
            " ↑↓←→ move  Tab panel  c copy  h hide H all  m mark M clear  d del  / search  s sort  r scan  ? help q".into()
        } else {
            " Arrows Tab h hide H all m mark M clear d del ? help q quit".into()
        };
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().fg(MUTED)),
            sections[6],
        );
    }
    // Keep operation feedback visible alongside selection details.
    if app.status.starts_with("Deletion")
        || app.status.starts_with("Scan failed")
        || app.status.contains("cannot be deleted")
        || app.status.contains("cannot be marked")
        || app.status.starts_with("Wait for")
        || app.status.starts_with("Clipboard")
    {
        let status_area = Rect::new(
            sections[5].x,
            sections[5].bottom() - 1,
            sections[5].width,
            1,
        );
        frame.render_widget(
            Paragraph::new(format!(" {}", app.status)).style(Style::default().fg(
                if app.status.starts_with("Clipboard:") {
                    ACCENT
                } else {
                    WARN
                },
            )),
            status_area,
        );
    }
    if app.modal.is_some() {
        draw_modal(frame, app);
    }
}

fn draw_tree(frame: &mut Frame, b: &mut Browser, area: Rect, hits: &mut HitAreas) {
    let match_info = if b.query.is_empty() {
        String::new()
    } else {
        let count = b
            .matches
            .iter()
            .filter(|&&id| b.data.is_within(id, b.scope))
            .count();
        format!(" · /{} · {count} matches", truncate(&b.query, 18))
    };
    let block = panel(
        format!(" 1 FILE TREE · {}{match_info} ", b.sort.label()),
        b.focus == Panel::Tree,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    hits.tree = area;
    let columns = Layout::horizontal([
        Constraint::Min(16),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(9),
    ])
    .split(inner);
    for (column, name) in columns
        .iter()
        .zip(["NAME", "SIZE", "% MAP", "USAGE", "FILES"])
    {
        frame.render_widget(
            Paragraph::new(name).style(Style::default().fg(MUTED)),
            Rect::new(column.x, inner.y, column.width, 1),
        );
    }
    let height = inner.height.saturating_sub(1) as usize;
    hits.page_size = height;
    b.ensure_visible(height);
    let total = b.visible_size(b.scope).max(1) as f64;
    for (offset, row) in b.rows.iter().skip(b.scroll).take(height).enumerate() {
        let y = inner.y + 1 + offset as u16;
        let node = &b.data.nodes[row.id];
        let hidden = b.is_hidden(row.id);
        let selected = row.id == b.selected;
        let style = Style::default()
            .bg(if selected { SELECTED } else { PANEL })
            .fg(if selected { TEXT } else { MUTED });
        frame.render_widget(
            Block::default().style(style),
            Rect::new(inner.x, y, inner.width, 1),
        );
        let repository = b.repositories.get(&row.id);
        let marked = b.marked.contains(&row.id);
        let mark_icon = if marked { "× " } else { "" };
        let git_icons = repository
            .map(|state| format!("{} ", state.icons()))
            .unwrap_or_default();
        let indent = (row.depth * 2).min(
            (columns[0].width as usize).saturating_sub(5 + git_icons.width() + mark_icon.width()),
        );
        let marker = match node.kind {
            Kind::Directory if b.expanded.contains(&row.id) || !b.query.is_empty() => "▾",
            Kind::Directory => "▸",
            Kind::Symlink => "↗",
            Kind::File => "·",
            Kind::Other => "?",
        };
        let suffix = if node.kind == Kind::Directory {
            "/"
        } else {
            ""
        };
        let name = if row.id == b.scope {
            display_name(b.data.path(row.id).as_os_str())
        } else {
            display_name(&node.name)
        };
        let prefix = format!(
            "{}{} {}",
            " ".repeat(indent),
            marker,
            if hidden { "[hidden] " } else { "" }
        );
        let name = format!(
            "{name}{suffix}{}",
            if node.error.is_some() { " !" } else { "" }
        );
        let name_style = if hidden {
            style.fg(MUTED).add_modifier(Modifier::DIM)
        } else if node.kind == Kind::Directory {
            style.fg(if selected { ACCENT } else { TEXT })
        } else {
            style
        };
        let width = (columns[0].width.saturating_sub(1) as usize)
            .saturating_sub(prefix.width() + git_icons.width() + mark_icon.width());
        let mut spans = vec![Span::styled(prefix, name_style)];
        if marked {
            spans.push(Span::styled(mark_icon, style.fg(WARN).bold()));
        }
        if let Some(repository) = repository {
            spans.push(Span::styled(git_icons, style.fg(git_color(repository))));
        }
        spans.push(Span::styled(truncate(&name, width), name_style));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(columns[0].x, y, columns[0].width, 1),
        );
        frame.render_widget(
            Paragraph::new(format!("{:>10}", human_size(node.size))).style(style),
            Rect::new(columns[1].x, y, columns[1].width, 1),
        );
        let ratio = b.visible_size(row.id) as f64 / total;
        frame.render_widget(
            Paragraph::new(format!("{:>6.1}%", ratio * 100.0)).style(style),
            Rect::new(columns[2].x, y, columns[2].width, 1),
        );
        let filled = (ratio * 10.0).round().clamp(0.0, 10.0) as usize;
        let bar = Line::from(vec![
            Span::styled(
                "━".repeat(filled),
                style.fg(if selected {
                    ACCENT
                } else {
                    color(&extension(node))
                }),
            ),
            Span::styled("─".repeat(10 - filled), style.fg(EDGE)),
        ]);
        frame.render_widget(
            Paragraph::new(bar),
            Rect::new(columns[3].x, y, columns[3].width, 1),
        );
        frame.render_widget(
            Paragraph::new(format!("{:>7}", node.files)).style(style),
            Rect::new(columns[4].x, y, columns[4].width, 1),
        );
        hits.rows.push((y, row.id, columns[0].x + indent as u16));
    }
    if b.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(" No matches. Esc clears the filter.").style(Style::default().fg(WARN)),
            Rect::new(
                inner.x,
                inner.y + 1,
                inner.width,
                inner.height.saturating_sub(1),
            ),
        );
    }
    if b.rows.len() > height && height > 0 {
        let position = format!(
            " {}–{} / {} ",
            b.scroll + 1,
            (b.scroll + height).min(b.rows.len()),
            b.rows.len()
        );
        let width = position.width().min(area.width.saturating_sub(4) as usize) as u16;
        frame.render_widget(
            Paragraph::new(position).style(Style::default().fg(MUTED).bg(PANEL)),
            Rect::new(area.right() - width - 2, area.bottom() - 1, width, 1),
        );
    }
}

fn draw_map(frame: &mut Frame, b: &mut Browser, area: Rect, hits: &mut HitAreas) {
    let scope = if b.scope == 0 {
        "all files".into()
    } else {
        display_name(&b.data.nodes[b.scope].name)
    };
    let hidden = b.hidden_in_scope();
    let title = if hidden > 0 {
        format!(
            " 2 SPACE MAP · {} shown · {hidden} hidden [H show all] ",
            human_size(b.visible_size(b.scope))
        )
    } else {
        format!(
            " 2 SPACE MAP · {} · Enter zoom · ⌫ out ",
            truncate(&scope, 30)
        )
    };
    let block = panel(title, b.focus == Panel::Map);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    hits.map = inner;
    b.layout_map(inner);
    if b.map.leaves.is_empty() {
        let message = if hidden > 0 {
            "\n No visible space to display.\n h toggles the selected item · H shows all hidden items."
        } else {
            "\n No space to display.\n Zero-byte files and empty directories remain in the tree."
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED)),
            inner,
        );
        return;
    }
    let has_selection = !b.rows.is_empty();
    let selection = b.map.rect(b.selected);
    for tile in &b.map.leaves {
        let node = &b.data.nodes[tile.id];
        let selected = has_selection
            && selection.is_some_and(|r| {
                r.contains(ratatui::layout::Position::new(tile.rect.x, tile.rect.y))
            });
        let base = color(&extension(node));
        let base = if selected { base } else { shade(base, 0.52) };
        for y in tile.rect.y..tile.rect.bottom() {
            for x in tile.rect.x..tile.rect.right() {
                let edge = x == tile.rect.x || y == tile.rect.y;
                frame.buffer_mut()[(x, y)]
                    .set_symbol(" ")
                    .set_bg(if edge { shade(base, 0.65) } else { base })
                    .set_fg(TEXT);
            }
        }
        if tile.rect.width >= 7 && tile.rect.height >= 2 {
            let label = truncate(
                &display_name(&node.name),
                tile.rect.width.saturating_sub(2) as usize,
            );
            let line = Line::from(Span::styled(
                label,
                Style::default()
                    .fg(if selected { Color::White } else { TEXT })
                    .bg(base)
                    .add_modifier(Modifier::BOLD),
            ));
            frame.buffer_mut().set_line(
                tile.rect.x + 1,
                tile.rect.y + u16::from(tile.rect.height >= 3),
                &line,
                tile.rect.width - 2,
            );
            if tile.rect.height >= 4 {
                let line = Line::from(Span::styled(
                    human_size(node.size),
                    Style::default()
                        .fg(if selected { Color::White } else { MUTED })
                        .bg(base),
                ));
                frame.buffer_mut().set_line(
                    tile.rect.x + 1,
                    tile.rect.y + 2,
                    &line,
                    tile.rect.width - 2,
                );
            }
        }
    }
    if has_selection && let Some(rect) = selection {
        if rect.width >= 2 && rect.height >= 2 {
            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::White).bold()),
                rect,
            );
        } else {
            for y in rect.y..rect.bottom() {
                for x in rect.x..rect.right() {
                    frame.buffer_mut()[(x, y)]
                        .set_symbol("▪")
                        .set_fg(Color::White)
                        .set_bg(ACCENT);
                }
            }
        }
    }
}

fn draw_legend(frame: &mut Frame, b: &Browser, area: Rect) {
    // Aggregate only visible leaves: bounded by terminal resolution, even on
    // multi-million-file scans. The map itself always uses the full byte totals.
    let mut types = BTreeMap::<String, u64>::new();
    for tile in &b.map.leaves {
        let node = &b.data.nodes[tile.id];
        *types.entry(extension(node)).or_default() += node.size;
    }
    let mut types: Vec<_> = types.into_iter().collect();
    types.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    let mut spans = vec![Span::styled(" visible types ", Style::default().fg(MUTED))];
    for (name, _) in types.into_iter().take(6) {
        spans.push(Span::styled("■ ", Style::default().fg(color(&name))));
        spans.push(Span::styled(
            format!("{}  ", truncate(&name, 10)),
            Style::default().fg(MUTED),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_modal(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let Some(modal) = app.modal.as_ref() else {
        return;
    };
    let (title, text, warning) = match modal {
        Modal::Help => (
            " KEYBOARD & MOUSE ",
            String::from(
                "NAVIGATION\n  Tab / Shift+Tab   Switch tree / map focus\n  ↑ ↓ ← → / jkl    Tree: move, collapse, expand\n                    Map: move between blocks spatially\n  PgUp / PgDn       Move a page in the tree\n  Home / End, g / G First / last visible tree item\n  Space             Toggle directory in tree\n  Enter             Zoom into selected directory\n                    On a map file: zoom into its parent\n  Backspace         Zoom out one directory\n\nSEARCH & ACTIONS\n  /                 Filter names (case insensitive)\n  Enter / Esc       Apply / clear search\n  n / N             Next / previous matching name\n  s                 Sort: largest, name, files, smallest\n  c                 Copy selected path to clipboard\n  r                 Rescan; preserve view where possible\n  h                 Hide / show selected item in the map\n  H                 Show all hidden items\n  m / M             Toggle deletion mark / clear all marks\n  d / Delete        Delete marked items, or selected item\n  e                 Show scan errors\n  ? / F1            This help\n  q / Ctrl+C        Quit (finishes any deletion first)\n\nMOUSE\n  Click tree row    Select item; click ▸ / ▾ to toggle\n  Click map block   Select and reveal item in the tree\n  Double-click map  Zoom into the block's parent\n  Right-click map   Zoom out\n  Wheel over tree   Move selection up / down\n\nThe tree and map share one selection. A selected directory\nhighlights all its blocks. Colors follow file extensions.\nTiny files can be below terminal resolution; zoom in to see\nthem. Search keeps the map context; h hides a subtree.\nHidden items stay in the tree; H shows everything again.\nMarked rows show ×; d/Delete confirms the marked batch.\nM clears all marks. Enter / Esc cancels confirmation.\n\nGIT ICONS (repository roots only)\n  ⎇ repository   … checking   ✓ clean and synced\n  * changes      ↑ ahead     ↓ behind\n  ∅ no remote    ? unverified / missing upstream\n  ◇ detached     ≡ stash present\nAll local branches are checked against last-fetched refs.\nNo network fetch; r rescans and refreshes the checks.\n\n↑/↓ or PgUp/PgDn scroll · Esc closes",
            ),
            false,
        ),
        Modal::Delete(request) => {
            let text = if request.items.len() == 1 {
                let item = &request.items[0];
                let kind = if item.kind == Kind::Directory {
                    "directory and all its contents"
                } else if item.kind == Kind::Symlink {
                    "symbolic link (target is kept)"
                } else {
                    "file"
                };
                format!(
                    "Delete this {kind}?\n\n{}\n\nReported usage: {} · {} files\n\nThis permanently removes the item; it does not use the trash.",
                    display_name(item.path.as_os_str()),
                    human_size(item.size),
                    item.files
                )
            } else {
                let paths = request
                    .items
                    .iter()
                    .map(|item| {
                        format!(
                            "{}{} · {}",
                            display_name(item.path.as_os_str()),
                            if item.kind == Kind::Directory {
                                "/"
                            } else {
                                ""
                            },
                            human_size(item.size)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "Delete these {} items?\n\n{paths}\n\nReported total: {} · {} files\n\nDeleting a directory includes everything inside it. Symbolic link targets are kept.\nThis permanently removes the items; it does not use the trash.",
                    request.items.len(),
                    human_size(request.size),
                    request.files
                )
            };
            (" CONFIRM PERMANENT DELETION ", text, true)
        }
        Modal::Errors => {
            let mut text = String::new();
            if let Some(b) = &app.browser {
                if b.data.errors == 0 {
                    text.push_str("No scan errors.\n");
                } else {
                    text.push_str(&format!(
                        "{} errors; totals may be incomplete.\n\n",
                        b.data.errors
                    ));
                }
                for (id, node) in b.data.nodes.iter().enumerate() {
                    if let Some(error) = &node.error {
                        text.push_str(&format!(
                            "{}\n  {error}\n\n",
                            display_name(b.data.path(id).as_os_str())
                        ));
                    }
                }
            }
            text.push_str("\n↑/↓ or PgUp/PgDn scroll · Esc closes");
            (" SCAN ERRORS ", text, false)
        }
    };
    let width = area.width.saturating_sub(6).min(82);
    let content_width = width.saturating_sub(4).max(1) as usize;
    let lines: usize = text
        .lines()
        .map(|line| line.width().max(1).div_ceil(content_width))
        .sum();
    let height =
        (lines + if warning { 6 } else { 4 }).min(area.height.saturating_sub(2) as usize) as u16;
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let mut block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if warning { WARN } else { ACCENT }))
        .style(Style::default().bg(PANEL).fg(TEXT));
    if lines + 4 > height as usize {
        block = block.title_bottom(" ↑↓ / PgUp/PgDn scroll ");
    }
    let inside = block.inner(rect);
    frame.render_widget(block, rect);
    let inside = Rect::new(
        inside.x + 1,
        inside.y + 1,
        inside.width.saturating_sub(2),
        inside.height.saturating_sub(if warning { 3 } else { 1 }),
    );
    let scroll =
        usize::from(app.modal_scroll).min(lines.saturating_sub(inside.height as usize)) as u16;
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(TEXT))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        inside,
    );
    if warning {
        frame.render_widget(
            Paragraph::new("[y] Delete permanently   [n / Esc / Enter] Cancel")
                .style(Style::default().fg(WARN).bold()),
            Rect::new(inside.x, rect.bottom() - 2, inside.width, 1),
        );
    }
}
