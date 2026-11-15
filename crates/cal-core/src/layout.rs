//! Panel layouts: a tree of row/column splits whose leaves are panels.
//!
//! In TOML (config `[layout]` or the state file):
//!
//! ```toml
//! [layout]
//! active = "mine"                                  # preset to use
//! narrow = { below_width = 70, preset = "stacked" } # small terminals
//!
//! [layout.presets.mine]
//! split = "columns"
//! children = [
//!   { panel = "appointments", size = "60%" },
//!   { split = "rows", children = [
//!       { panel = "calendar", size = 9 },          # 9 cells
//!       { panel = "todo" },                        # no size: the rest
//!   ]},
//! ]
//! ```
//!
//! Precedence: built-in presets < presets saved from the TUI (state file) <
//! presets in the config. `active` in the config beats the state file, so a
//! layout declared in Nix stays put.

use std::collections::BTreeMap;
use std::path::Path;

use toml::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

    pub fn name(self) -> &'static str {
        match self {
            PanelKind::Appointments => "appointments",
            PanelKind::Calendar => "calendar",
            PanelKind::Todo => "todo",
        }
    }

    pub fn from_name(s: &str) -> Option<PanelKind> {
        PanelKind::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// Split direction: `rows` stacks children top to bottom, `columns` puts them
/// side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Rows,
    Columns,
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
    Split(Dir, Size, Vec<Node>),
}

/// Height of the month calendar: border, weekday header, six weeks, border.
pub const CALENDAR_HEIGHT: u16 = 9;

/// Names of the built-in presets, in cycling order.
pub const BUILTIN: [&str; 4] = ["classic", "classic-left", "three-columns", "stacked"];

impl Node {
    pub fn size(&self) -> Size {
        match self {
            Node::Panel(_, s) | Node::Split(_, s, _) => *s,
        }
    }

    pub fn set_size(&mut self, size: Size) {
        match self {
            Node::Panel(_, s) | Node::Split(_, s, _) => *s = size,
        }
    }

    /// A built-in preset by name.
    pub fn builtin(name: &str) -> Option<Node> {
        use PanelKind::*;
        let side = || {
            Node::Split(
                Dir::Rows,
                Size::Cells(32),
                vec![
                    Node::Panel(Calendar, Size::Cells(CALENDAR_HEIGHT)),
                    Node::Panel(Todo, Size::Fill),
                ],
            )
        };
        Some(match name {
            // calcurse's default: appointments left, calendar over todo right.
            "classic" => Node::Split(
                Dir::Columns,
                Size::Fill,
                vec![Node::Panel(Appointments, Size::Fill), side()],
            ),
            "classic-left" => Node::Split(
                Dir::Columns,
                Size::Fill,
                vec![side(), Node::Panel(Appointments, Size::Fill)],
            ),
            "three-columns" => Node::Split(
                Dir::Columns,
                Size::Fill,
                vec![
                    Node::Panel(Calendar, Size::Cells(32)),
                    Node::Panel(Appointments, Size::Fill),
                    Node::Panel(Todo, Size::Percent(30)),
                ],
            ),
            // One column, for narrow terminals.
            "stacked" => Node::Split(
                Dir::Rows,
                Size::Fill,
                vec![
                    Node::Panel(Calendar, Size::Cells(CALENDAR_HEIGHT)),
                    Node::Panel(Appointments, Size::Fill),
                    Node::Panel(Todo, Size::Percent(30)),
                ],
            ),
            _ => return None,
        })
    }

    /// Panels in the tree, in order.
    pub fn panels(&self) -> Vec<PanelKind> {
        match self {
            Node::Panel(p, _) => vec![*p],
            Node::Split(_, _, c) => c.iter().flat_map(Node::panels).collect(),
        }
    }

    /// Parse from TOML; `path` names the location for error messages.
    pub fn from_toml(v: &Value, path: &str) -> Result<Node, String> {
        let node = parse_node(v, path)?;
        let panels = node.panels();
        for p in PanelKind::ALL {
            if panels.iter().filter(|q| **q == p).count() > 1 {
                return Err(format!(
                    "{path}: panel '{}' appears more than once",
                    p.name()
                ));
            }
        }
        Ok(node)
    }

    pub fn to_toml(&self) -> Value {
        let mut t = toml::map::Map::new();
        match self {
            Node::Panel(p, _) => {
                t.insert("panel".into(), Value::String(p.name().into()));
            }
            Node::Split(d, _, children) => {
                let d = match d {
                    Dir::Rows => "rows",
                    Dir::Columns => "columns",
                };
                t.insert("split".into(), Value::String(d.into()));
                t.insert(
                    "children".into(),
                    Value::Array(children.iter().map(Node::to_toml).collect()),
                );
            }
        }
        match self.size() {
            Size::Fill => {}
            Size::Percent(p) => {
                t.insert("size".into(), Value::String(format!("{p}%")));
            }
            Size::Cells(n) => {
                t.insert("size".into(), Value::Integer(n as i64));
            }
        }
        Value::Table(t)
    }
}

fn parse_size(v: Option<&Value>, path: &str) -> Result<Size, String> {
    match v {
        None => Ok(Size::Fill),
        Some(Value::Integer(n)) if (1..=1000).contains(n) => Ok(Size::Cells(*n as u16)),
        Some(Value::String(s)) => match s.trim().strip_suffix('%').map(|p| p.trim().parse::<u16>())
        {
            Some(Ok(p)) if (1..=100).contains(&p) => Ok(Size::Percent(p)),
            _ => Err(format!(
                "{path}.size: '{s}' is not a percentage like \"60%\""
            )),
        },
        Some(other) => Err(format!(
            "{path}.size: {other} is not a number of cells (1-1000) or a percentage like \"60%\""
        )),
    }
}

fn parse_node(v: &Value, path: &str) -> Result<Node, String> {
    let t = v
        .as_table()
        .ok_or_else(|| format!("{path}: expected a table like {{ panel = \"todo\" }}"))?;
    for k in t.keys() {
        if !matches!(k.as_str(), "panel" | "split" | "size" | "children") {
            return Err(format!("{path}: unknown key '{k}'"));
        }
    }
    let size = parse_size(t.get("size"), path)?;
    match (t.get("panel"), t.get("split")) {
        (Some(p), None) => {
            if t.contains_key("children") {
                return Err(format!("{path}: a panel cannot have children"));
            }
            let name = p.as_str().unwrap_or_default();
            let kind = PanelKind::from_name(name).ok_or_else(|| {
                format!("{path}.panel: unknown panel '{p}' (appointments, calendar, todo)")
            })?;
            Ok(Node::Panel(kind, size))
        }
        (None, Some(s)) => {
            let dir = match s.as_str() {
                Some("rows") => Dir::Rows,
                Some("columns") => Dir::Columns,
                _ => {
                    return Err(format!(
                        "{path}.split: '{s}' must be \"rows\" or \"columns\""
                    ))
                }
            };
            let children = t
                .get("children")
                .and_then(Value::as_array)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| format!("{path}: a split needs a non-empty children list"))?;
            let children = children
                .iter()
                .enumerate()
                .map(|(i, c)| parse_node(c, &format!("{path}.children[{i}]")))
                .collect::<Result<Vec<_>, _>>()?;
            let pct: u16 = children
                .iter()
                .map(|c| match c.size() {
                    Size::Percent(p) => p,
                    _ => 0,
                })
                .sum();
            if pct > 100 {
                return Err(format!("{path}: children sizes add up to {pct}%"));
            }
            Ok(Node::Split(dir, size, children))
        }
        (Some(_), Some(_)) => Err(format!("{path}: has both 'panel' and 'split'")),
        (None, None) => Err(format!("{path}: needs 'panel' or 'split'")),
    }
}

/// `[layout]` from the config, or the TUI's state file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutConfig {
    pub active: Option<String>,
    /// Below this terminal width, use this preset.
    pub narrow: Option<(u16, String)>,
    pub presets: BTreeMap<String, Node>,
}

impl LayoutConfig {
    pub fn from_toml(v: &Value, path: &str) -> Result<LayoutConfig, String> {
        let t = v
            .as_table()
            .ok_or_else(|| format!("{path}: expected a table"))?;
        let mut out = LayoutConfig::default();
        for (k, v) in t {
            match k.as_str() {
                "active" => {
                    out.active = Some(
                        v.as_str()
                            .ok_or_else(|| format!("{path}.active: expected a preset name"))?
                            .to_string(),
                    )
                }
                "narrow" => {
                    let n = v.as_table();
                    let w = n
                        .and_then(|n| n.get("below_width"))
                        .and_then(Value::as_integer);
                    let p = n.and_then(|n| n.get("preset")).and_then(Value::as_str);
                    match (w, p, n.map(|n| n.len())) {
                        (Some(w), Some(p), Some(2)) if (1..=1000).contains(&w) => {
                            out.narrow = Some((w as u16, p.to_string()))
                        }
                        _ => {
                            let want = "{ below_width = 70, preset = \"stacked\" }";
                            return Err(format!("{path}.narrow: expected {want}"));
                        }
                    }
                }
                "presets" => {
                    let ps = v
                        .as_table()
                        .ok_or_else(|| format!("{path}.presets: expected a table of presets"))?;
                    for (name, node) in ps {
                        out.presets.insert(
                            name.clone(),
                            Node::from_toml(node, &format!("{path}.presets.{name}"))?,
                        );
                    }
                }
                other => return Err(format!("{path}: unknown key '{other}'")),
            }
        }
        // Names must exist (built-in or defined here).
        let known = |n: &str| BUILTIN.contains(&n) || out.presets.contains_key(n);
        if let Some(a) = &out.active {
            if !known(a) {
                return Err(format!("{path}.active: no preset named '{a}'"));
            }
        }
        if let Some((_, p)) = &out.narrow {
            if !known(p) {
                return Err(format!("{path}.narrow.preset: no preset named '{p}'"));
            }
        }
        Ok(out)
    }

    pub fn to_toml(&self) -> Value {
        let mut t = toml::map::Map::new();
        if let Some(a) = &self.active {
            t.insert("active".into(), Value::String(a.clone()));
        }
        if let Some((w, p)) = &self.narrow {
            let mut n = toml::map::Map::new();
            n.insert("below_width".into(), Value::Integer(*w as i64));
            n.insert("preset".into(), Value::String(p.clone()));
            t.insert("narrow".into(), Value::Table(n));
        }
        if !self.presets.is_empty() {
            let ps = self
                .presets
                .iter()
                .map(|(k, v)| (k.clone(), v.to_toml()))
                .collect();
            t.insert("presets".into(), Value::Table(ps));
        }
        Value::Table(t)
    }

    /// Read the TUI's saved layouts (`[layout]` in the file). Missing file:
    /// empty; broken file: error (the caller reports it and ignores it).
    pub fn load_state(path: &Path) -> Result<LayoutConfig, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let v: Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        match v.get("layout") {
            Some(l) => Self::from_toml(l, "layout").map_err(|e| format!("{}: {e}", path.display())),
            None => Ok(Self::default()),
        }
    }

    pub fn save_state(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut root = toml::map::Map::new();
        root.insert("layout".into(), self.to_toml());
        let text = format!(
            "# Written by cal-tui when you save a layout in the TUI (w, then Enter).\n\
             # A [layout] in config.toml takes precedence over this file.\n\n{}",
            toml::to_string(&Value::Table(root)).unwrap_or_default()
        );
        crate::fsio::write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<LayoutConfig, String> {
        let v: Value = toml::from_str(s).unwrap();
        LayoutConfig::from_toml(v.get("layout").unwrap(), "layout")
    }

    #[test]
    fn parses_the_documented_example() {
        let l = parse(
            r#"
            [layout]
            active = "mine"
            narrow = { below_width = 70, preset = "stacked" }
            [layout.presets.mine]
            split = "columns"
            children = [
              { panel = "appointments", size = "60%" },
              { split = "rows", children = [
                  { panel = "calendar", size = 9 },
                  { panel = "todo" },
              ]},
            ]
            "#,
        )
        .unwrap();
        assert_eq!(l.active.as_deref(), Some("mine"));
        assert_eq!(l.narrow, Some((70, "stacked".into())));
        let mine = &l.presets["mine"];
        assert_eq!(mine.panels(), PanelKind::ALL.to_vec());
        // Round trip.
        let back = LayoutConfig::from_toml(&l.to_toml(), "layout").unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn builtins_round_trip_and_contain_every_panel() {
        for name in BUILTIN {
            let n = Node::builtin(name).unwrap();
            let mut p = n.panels();
            p.sort();
            assert_eq!(p, PanelKind::ALL.to_vec(), "{name}");
            assert_eq!(Node::from_toml(&n.to_toml(), name).unwrap(), n);
        }
    }

    #[test]
    fn helpful_errors() {
        let err = |s: &str| parse(&format!("[layout]\n{s}")).unwrap_err();
        assert_eq!(
            err("active = \"nope\""),
            "layout.active: no preset named 'nope'"
        );
        assert!(err("[layout.presets.x]\npanel = \"agenda\"").contains("unknown panel"));
        assert!(
            err("[layout.presets.x]\nsplit = \"diagonal\"\nchildren = [{ panel = \"todo\" }]")
                .contains("must be \"rows\" or \"columns\"")
        );
        assert!(err("[layout.presets.x]\nsplit = \"rows\"\nchildren = [{ panel = \"todo\", size = \"60\" }]")
            .contains("percentage"));
        assert!(err("[layout.presets.x]\nsplit = \"rows\"\nchildren = [{ panel = \"todo\" }, { panel = \"todo\" }]")
            .contains("more than once"));
        assert!(err("[layout.presets.x]\nsplit = \"rows\"\nchildren = [{ panel = \"todo\", size = \"70%\" }, { panel = \"calendar\", size = \"40%\" }]")
            .contains("add up to 110%"));
        assert!(err("colour = 1").contains("unknown key 'colour'"));
    }
}
