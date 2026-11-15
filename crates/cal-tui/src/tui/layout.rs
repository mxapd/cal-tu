//! Panel layout as a tree of row/column splits.
//!
//! Milestone 5 will load presets from the config and allow rearranging them in
//! the TUI; for now the built-in presets are used.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Appointments,
    Calendar,
    Todo,
}

impl PanelKind {
    pub const ALL: [PanelKind; 3] = [
        PanelKind::Appointments,
        PanelKind::Calendar,
        PanelKind::Todo,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Share the remaining space.
    Fill,
    Percent(u16),
    /// Fixed number of terminal cells.
    Cells(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Panel(PanelKind, Size),
    Split(Direction, Size, Vec<Node>),
}

/// Height of the calendar panel: border, weekday header, six weeks, border.
pub const CALENDAR_HEIGHT: u16 = 9;

impl Node {
    fn size(&self) -> Size {
        match self {
            Node::Panel(_, s) | Node::Split(_, s, _) => *s,
        }
    }

    /// calcurse's default: appointments on the left, calendar above todo on
    /// the right.
    pub fn classic() -> Node {
        Node::Split(
            Direction::Horizontal,
            Size::Fill,
            vec![
                Node::Panel(PanelKind::Appointments, Size::Fill),
                Node::Split(
                    Direction::Vertical,
                    Size::Cells(32),
                    vec![
                        Node::Panel(PanelKind::Calendar, Size::Cells(CALENDAR_HEIGHT)),
                        Node::Panel(PanelKind::Todo, Size::Fill),
                    ],
                ),
            ],
        )
    }

    /// Single column for narrow terminals.
    pub fn stacked() -> Node {
        Node::Split(
            Direction::Vertical,
            Size::Fill,
            vec![
                Node::Panel(PanelKind::Calendar, Size::Cells(CALENDAR_HEIGHT)),
                Node::Panel(PanelKind::Appointments, Size::Fill),
                Node::Panel(PanelKind::Todo, Size::Percent(30)),
            ],
        )
    }

    /// Compute the screen area of every panel in the tree.
    pub fn resolve(&self, area: Rect, out: &mut Vec<(PanelKind, Rect)>) {
        match self {
            Node::Panel(kind, _) => out.push((*kind, area)),
            Node::Split(dir, _, children) => {
                let constraints: Vec<Constraint> = children
                    .iter()
                    .map(|c| match c.size() {
                        Size::Fill => Constraint::Fill(1),
                        Size::Percent(p) => Constraint::Percentage(p),
                        Size::Cells(n) => Constraint::Length(n),
                    })
                    .collect();
                let rects = Layout::new(*dir, constraints).split(area);
                for (child, rect) in children.iter().zip(rects.iter()) {
                    child.resolve(*rect, out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_layout_places_all_panels() {
        let mut out = Vec::new();
        Node::classic().resolve(Rect::new(0, 0, 100, 30), &mut out);
        let get = |k| out.iter().find(|(p, _)| *p == k).unwrap().1;
        assert_eq!(get(PanelKind::Appointments), Rect::new(0, 0, 68, 30));
        assert_eq!(get(PanelKind::Calendar), Rect::new(68, 0, 32, 9));
        assert_eq!(get(PanelKind::Todo), Rect::new(68, 9, 32, 21));
    }
}
