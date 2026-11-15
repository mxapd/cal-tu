//! Placing panels on screen, and the layout-mode operations (resize, swap,
//! rotate, hide, unhide). The layout model lives in `cal_core::layout`.

pub use cal_core::layout::{Dir, Node, PanelKind, Size};
use ratatui::layout::{Constraint, Direction, Layout, Rect};

fn direction(d: Dir) -> Direction {
    match d {
        Dir::Rows => Direction::Vertical,
        Dir::Columns => Direction::Horizontal,
    }
}

fn split_rects(dir: Dir, children: &[Node], area: Rect) -> Vec<Rect> {
    let constraints: Vec<Constraint> = children
        .iter()
        .map(|c| match c.size() {
            Size::Fill => Constraint::Fill(1),
            Size::Percent(p) => Constraint::Percentage(p),
            Size::Cells(n) => Constraint::Length(n),
        })
        .collect();
    Layout::new(direction(dir), constraints)
        .split(area)
        .to_vec()
}

/// Screen area of every panel in the tree.
pub fn resolve(node: &Node, area: Rect, out: &mut Vec<(PanelKind, Rect)>) {
    match node {
        Node::Panel(kind, _) => out.push((*kind, area)),
        Node::Split(dir, _, children) => {
            for (child, rect) in children.iter().zip(split_rects(*dir, children, area)) {
                resolve(child, rect, out);
            }
        }
    }
}

/// Child indices leading to `panel`.
fn path_to(node: &Node, panel: PanelKind) -> Option<Vec<usize>> {
    match node {
        Node::Panel(p, _) => (*p == panel).then(Vec::new),
        Node::Split(_, _, children) => children.iter().enumerate().find_map(|(i, c)| {
            let mut p = path_to(c, panel)?;
            p.insert(0, i);
            Some(p)
        }),
    }
}

fn node_at<'a>(node: &'a Node, path: &[usize]) -> &'a Node {
    match (path.split_first(), node) {
        (Some((i, rest)), Node::Split(_, _, c)) => node_at(&c[*i], rest),
        (_, n) => n,
    }
}

fn node_at_mut<'a>(node: &'a mut Node, path: &[usize]) -> &'a mut Node {
    match (path.split_first(), node) {
        (None, n) => n,
        (Some((i, rest)), Node::Split(_, _, c)) => node_at_mut(&mut c[*i], rest),
        (Some(_), n) => n,
    }
}

/// Screen area of the node at `path`.
fn rect_at(node: &Node, area: Rect, path: &[usize]) -> Rect {
    match (path.split_first(), node) {
        (Some((i, rest)), Node::Split(dir, _, c)) => {
            rect_at(&c[*i], split_rects(*dir, c, area)[*i], rest)
        }
        _ => area,
    }
}

/// Grow (`delta > 0`) or shrink the focused panel by `delta` percent of its
/// split. `along = false` resizes the split that contains the panel instead
/// (e.g. the whole right-hand column). Returns false if there is nothing to
/// resize.
pub fn resize(root: &mut Node, panel: PanelKind, along: bool, delta: i32, area: Rect) -> bool {
    let Some(mut path) = path_to(root, panel) else {
        return false;
    };
    if !along {
        path.pop();
    }
    let Some((_, parent_path)) = path.split_last() else {
        return false; // the root has no siblings
    };
    let Node::Split(dir, _, siblings) = node_at(root, parent_path).clone() else {
        return false;
    };
    if siblings.len() < 2 {
        return false;
    }
    let parent = rect_at(root, area, parent_path);
    let own = rect_at(root, area, &path);
    let (have, total) = match dir {
        Dir::Columns => (own.width, parent.width),
        Dir::Rows => (own.height, parent.height),
    };
    if total == 0 {
        return false;
    }
    let pct = ((have as i32 * 100 + total as i32 / 2) / total as i32 + delta).clamp(10, 90);
    let me = *path.last().expect("non-empty");
    let Node::Split(_, _, children) = node_at_mut(root, parent_path) else {
        return false;
    };
    // Fixed-size neighbours would override the new size: let them take the
    // remaining space instead.
    for (i, c) in children.iter_mut().enumerate() {
        if i != me && matches!(c.size(), Size::Cells(_)) {
            c.set_size(Size::Fill);
        }
    }
    // Keep the split's percentages within 100, leaving every flexible
    // neighbour at least 5%: shrink percentage neighbours proportionally.
    let fills = children
        .iter()
        .enumerate()
        .filter(|(i, c)| *i != me && c.size() == Size::Fill)
        .count() as i32;
    let others: i32 = children
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != me)
        .map(|(_, c)| match c.size() {
            Size::Percent(p) => p as i32,
            _ => 0,
        })
        .sum();
    let room = 100 - pct - 5 * fills;
    if others > room {
        let n_pct = children
            .iter()
            .enumerate()
            .filter(|(i, c)| *i != me && matches!(c.size(), Size::Percent(_)))
            .count() as i32;
        let room = room.max(5 * n_pct);
        for (i, c) in children.iter_mut().enumerate() {
            if let (true, Size::Percent(p)) = (i != me, c.size()) {
                c.set_size(Size::Percent(((p as i32 * room) / others).max(5) as u16));
            }
        }
    }
    children[me].set_size(Size::Percent(pct as u16));
    true
}

/// Move the divider after `panel` (below it for `Dir::Rows`, to its right for
/// `Dir::Columns`) to screen coordinate `pos`. The divider belongs to the
/// nearest split in that direction where the panel (or the group holding it)
/// is not the last child; that child gets a fixed size in cells.
pub fn drag_divider(root: &mut Node, panel: PanelKind, dir: Dir, pos: u16, area: Rect) -> bool {
    let Some(mut path) = path_to(root, panel) else {
        return false;
    };
    while let Some((&i, parent)) = path.split_last() {
        if let Node::Split(d, _, siblings) = node_at(root, parent) {
            if *d == dir && i + 1 < siblings.len() {
                let own = rect_at(root, area, &path);
                let whole = rect_at(root, area, parent);
                let (start, total) = match dir {
                    Dir::Rows => (own.y, whole.height),
                    Dir::Columns => (own.x, whole.width),
                };
                // Keep at least 3 cells on either side.
                let size = (pos + 1)
                    .saturating_sub(start)
                    .clamp(3, total.saturating_sub(3).max(3));
                let parent = parent.to_vec();
                if let Node::Split(_, _, children) = node_at_mut(root, &parent) {
                    children[i].set_size(Size::Cells(size));
                    // Someone has to take the remaining space.
                    if !children
                        .iter()
                        .enumerate()
                        .any(|(j, c)| j != i && c.size() == Size::Fill)
                    {
                        children[i + 1].set_size(Size::Fill);
                    }
                }
                return true;
            }
        }
        path.pop();
    }
    false
}

/// Move the focused panel one place forward in its split (wrapping).
pub fn swap(root: &mut Node, panel: PanelKind) -> bool {
    let Some(path) = path_to(root, panel) else {
        return false;
    };
    let Some((i, parent)) = path.split_last() else {
        return false;
    };
    match node_at_mut(root, parent) {
        Node::Split(_, _, c) if c.len() > 1 => {
            let j = (i + 1) % c.len();
            c.swap(*i, j);
            true
        }
        _ => false,
    }
}

/// Turn the split holding the focused panel from rows into columns or back.
pub fn rotate(root: &mut Node, panel: PanelKind) -> bool {
    let Some(path) = path_to(root, panel) else {
        return false;
    };
    let Some((_, parent)) = path.split_last() else {
        return false;
    };
    match node_at_mut(root, parent) {
        Node::Split(d, _, _) => {
            *d = match d {
                Dir::Rows => Dir::Columns,
                Dir::Columns => Dir::Rows,
            };
            true
        }
        _ => false,
    }
}

/// Remove a panel from the layout (not the last one).
pub fn hide(root: &mut Node, panel: PanelKind) -> bool {
    fn remove(node: &mut Node, panel: PanelKind) -> bool {
        if let Node::Split(_, _, c) = node {
            if let Some(i) = c
                .iter()
                .position(|n| matches!(n, Node::Panel(p, _) if *p == panel))
            {
                c.remove(i);
                return true;
            }
            for child in c.iter_mut() {
                if remove(child, panel) {
                    break;
                }
            }
            // Drop splits that became empty.
            c.retain(|n| !matches!(n, Node::Split(_, _, cc) if cc.is_empty()));
            return true;
        }
        false
    }
    if root.panels().len() <= 1 || path_to(root, panel).is_none() {
        return false;
    }
    remove(root, panel)
}

/// Add hidden panels back, at the end of the outermost split.
pub fn unhide(root: &mut Node) -> bool {
    let missing: Vec<PanelKind> = PanelKind::ALL
        .into_iter()
        .filter(|p| !root.panels().contains(p))
        .collect();
    if missing.is_empty() {
        return false;
    }
    if let Node::Panel(..) = root {
        let only = std::mem::replace(root, Node::Split(Dir::Columns, Size::Fill, Vec::new()));
        if let Node::Split(_, _, c) = root {
            c.push(only);
        }
    }
    if let Node::Split(_, _, c) = root {
        for p in missing {
            c.push(Node::Panel(p, Size::Fill));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(n: &Node, w: u16, h: u16) -> Vec<(PanelKind, Rect)> {
        let mut out = Vec::new();
        resolve(n, Rect::new(0, 0, w, h), &mut out);
        out
    }

    fn get(r: &[(PanelKind, Rect)], k: PanelKind) -> Rect {
        r.iter().find(|(p, _)| *p == k).unwrap().1
    }

    #[test]
    fn classic_layout_places_all_panels() {
        let r = rects(&Node::builtin("classic").unwrap(), 100, 30);
        assert_eq!(get(&r, PanelKind::Appointments), Rect::new(0, 0, 62, 30));
        assert_eq!(get(&r, PanelKind::Calendar), Rect::new(62, 0, 38, 17));
        assert_eq!(get(&r, PanelKind::Todo), Rect::new(62, 17, 38, 13));
    }

    #[test]
    fn resize_along_and_across() {
        let area = Rect::new(0, 0, 100, 30);
        let mut n = Node::builtin("classic").unwrap();
        // Appointments 62% → 67%.
        assert!(resize(&mut n, PanelKind::Appointments, true, 5, area));
        assert_eq!(get(&rects(&n, 100, 30), PanelKind::Appointments).width, 67);
        // Calendar taller within its column.
        assert!(resize(&mut n, PanelKind::Calendar, true, 10, area));
        assert!(get(&rects(&n, 100, 30), PanelKind::Calendar).height > 17);
        // Across: the right-hand column (calendar + todo) gets wider.
        let before = get(&rects(&n, 100, 30), PanelKind::Todo).width;
        assert!(resize(&mut n, PanelKind::Todo, false, 10, area));
        assert!(get(&rects(&n, 100, 30), PanelKind::Todo).width > before);
    }

    #[test]
    fn swap_rotate_hide_unhide() {
        let mut n = Node::builtin("classic").unwrap();
        assert!(swap(&mut n, PanelKind::Appointments));
        assert_eq!(n, Node::builtin("classic-left").unwrap());
        assert!(rotate(&mut n, PanelKind::Calendar));
        let r = rects(&n, 100, 30);
        assert_eq!(
            get(&r, PanelKind::Calendar).y,
            get(&r, PanelKind::Todo).y,
            "side by side"
        );
        assert!(hide(&mut n, PanelKind::Todo));
        assert!(!n.panels().contains(&PanelKind::Todo));
        assert!(hide(&mut n, PanelKind::Calendar));
        assert!(!hide(&mut n, PanelKind::Appointments), "keep at least one");
        assert_eq!(n.panels(), vec![PanelKind::Appointments]);
        assert!(unhide(&mut n));
        let mut p = n.panels();
        p.sort();
        assert_eq!(p, PanelKind::ALL.to_vec());
    }
}
