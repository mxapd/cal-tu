//! Key bindings: which keys trigger which TUI action. Frontend-independent
//! (no terminal library), so `check-config` can validate them.
//!
//! In TOML (config `[keys]`):
//!
//! ```toml
//! [keys]                      # main view
//! next_day = ["l", "Right"]   # replaces the default keys of that action
//! quit = "Q"                  # one key
//! sync = []                   # unbound
//!
//! [keys.layout]               # layout mode
//! save = "ctrl+s"
//!
//! [keys.popup]                # pop-ups (help, details, lists, questions)
//! [keys.form]                 # add/edit form and the go-to-date prompt
//! ```
//!
//! Actions not mentioned keep their defaults. `Ctrl-C` always quits and is
//! not part of the map.

use std::collections::BTreeMap;
use std::fmt;

use toml::Value;

/// A key without modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A printable character; `' '` is Space. Letters keep their case, so
    /// `A` means Shift+a.
    Char(char),
    Enter,
    Esc,
    Tab,
    /// Shift+Tab.
    BackTab,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// A key with modifiers. Shift is folded into `Char` (as the case of the
/// character) and into `BackTab`, so it only shows up on other keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Named keys: (config name, alternative spellings).
const NAMED: [(Key, &str, &[&str]); 16] = [
    (Key::Enter, "Enter", &["return"]),
    (Key::Esc, "Esc", &["escape"]),
    (Key::Tab, "Tab", &[]),
    (Key::BackTab, "BackTab", &[]),
    (Key::Backspace, "Backspace", &[]),
    (Key::Delete, "Delete", &["del"]),
    (Key::Insert, "Insert", &["ins"]),
    (Key::Left, "Left", &[]),
    (Key::Right, "Right", &[]),
    (Key::Up, "Up", &[]),
    (Key::Down, "Down", &[]),
    (Key::Home, "Home", &[]),
    (Key::End, "End", &[]),
    (Key::PageUp, "PageUp", &["pgup"]),
    (Key::PageDown, "PageDown", &["pgdn"]),
    (Key::Char(' '), "Space", &[]),
];

impl KeyChord {
    pub fn new(key: Key, ctrl: bool, alt: bool, shift: bool) -> KeyChord {
        KeyChord {
            key,
            ctrl,
            alt,
            shift,
        }
        .normalized()
    }

    pub fn plain(key: Key) -> KeyChord {
        KeyChord::new(key, false, false, false)
    }

    /// One canonical form per physical key, so lookups compare equal.
    fn normalized(mut self) -> KeyChord {
        match &mut self.key {
            Key::Char(c) => {
                if self.shift && c.is_lowercase() {
                    *c = c.to_uppercase().next().unwrap_or(*c);
                }
                // Terminals do not tell Ctrl-S from Ctrl-Shift-S.
                if self.ctrl {
                    *c = c.to_lowercase().next().unwrap_or(*c);
                }
                self.shift = false;
            }
            Key::BackTab => self.shift = false,
            Key::Tab if self.shift => {
                self.key = Key::BackTab;
                self.shift = false;
            }
            _ => {}
        }
        self
    }

    /// Parse `a`, `A`, `+`, `ctrl+s`, `alt+x`, `shift+tab`, `Enter`, `F5`, …
    /// Modifier and key names ignore case; single characters do not.
    pub fn parse(s: &str) -> Result<KeyChord, String> {
        let bad = || format!("cannot parse key '{s}'");
        let s = s.trim();
        if s.is_empty() {
            return Err("empty key".into());
        }
        // The key is after the last '+', unless the key itself is '+'.
        let (mods, key) = if s == "+" {
            ("", "+")
        } else if let Some(m) = s.strip_suffix("++") {
            (m, "+")
        } else {
            match s.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", s),
            }
        };
        if key.is_empty() {
            return Err(bad());
        }
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        if !mods.is_empty() {
            for m in mods.split('+') {
                match m.to_ascii_lowercase().as_str() {
                    "ctrl" | "control" => ctrl = true,
                    "alt" | "meta" => alt = true,
                    "shift" => shift = true,
                    _ => return Err(format!("{}: unknown modifier '{m}'", bad())),
                }
            }
        }
        let mut chars = key.chars();
        let k = match (chars.next(), chars.next()) {
            (Some(c), None) => Key::Char(c),
            _ => {
                let lower = key.to_ascii_lowercase();
                if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    if !(1..=24).contains(&n) {
                        return Err(bad());
                    }
                    Key::F(n)
                } else {
                    NAMED
                        .iter()
                        .find(|(_, name, alts)| {
                            name.eq_ignore_ascii_case(&lower) || alts.contains(&lower.as_str())
                        })
                        .map(|(k, _, _)| *k)
                        .ok_or_else(bad)?
                }
            }
        };
        Ok(KeyChord::new(k, ctrl, alt, shift))
    }

    /// The config spelling (`ctrl+s`, `Left`, `Space`), parseable again.
    pub fn spec(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        if self.shift {
            s.push_str("shift+");
        }
        match self.key {
            Key::Char(' ') => s.push_str("Space"),
            Key::Char(c) => s.push(c),
            Key::F(n) => s.push_str(&format!("F{n}")),
            k => s.push_str(NAMED.iter().find(|(n, _, _)| *n == k).map_or("?", |n| n.1)),
        }
        s
    }
}

/// How keys are shown in help and hints: `Ctrl-S`, `←`, `Shift-Tab`, `PgDn`.
impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl-")?;
        }
        if self.alt {
            f.write_str("Alt-")?;
        }
        if self.shift {
            f.write_str("Shift-")?;
        }
        match self.key {
            Key::Char(' ') => f.write_str("Space"),
            Key::Char(c) if self.ctrl => write!(f, "{}", c.to_uppercase()),
            Key::Char(c) => write!(f, "{c}"),
            Key::Enter => f.write_str("Enter"),
            Key::Esc => f.write_str("Esc"),
            Key::Tab => f.write_str("Tab"),
            Key::BackTab => f.write_str("Shift-Tab"),
            Key::Backspace => f.write_str("Backspace"),
            Key::Delete => f.write_str("Del"),
            Key::Insert => f.write_str("Ins"),
            Key::Left => f.write_str("←"),
            Key::Right => f.write_str("→"),
            Key::Up => f.write_str("↑"),
            Key::Down => f.write_str("↓"),
            Key::Home => f.write_str("Home"),
            Key::End => f.write_str("End"),
            Key::PageUp => f.write_str("PgUp"),
            Key::PageDown => f.write_str("PgDn"),
            Key::F(n) => write!(f, "F{n}"),
        }
    }
}

/// Where an action applies; also the `[keys.*]` table it is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    /// The main view, `[keys]` itself.
    Main,
    Layout,
    Popup,
    Form,
}

impl Context {
    pub const ALL: [Context; 4] = [
        Context::Main,
        Context::Layout,
        Context::Popup,
        Context::Form,
    ];

    /// The sub-table name (`None` for the main view).
    pub fn table(self) -> Option<&'static str> {
        match self {
            Context::Main => None,
            Context::Layout => Some("layout"),
            Context::Popup => Some("popup"),
            Context::Form => Some("form"),
        }
    }

    fn path(self) -> String {
        match self.table() {
            Some(t) => format!("keys.{t}"),
            None => "keys".into(),
        }
    }
}

macro_rules! actions {
    ($($ctx:ident { $($var:ident $name:literal [$($k:literal),*])* })*) => {
        /// Everything a key can be bound to.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Action { $($($var,)*)* }

        /// (action, context, config name, default keys), in help order.
        const TABLE: &[(Action, Context, &str, &[&str])] = &[
            $($((Action::$var, Context::$ctx, $name, &[$($k),*]),)*)*
        ];
    };
}

actions! {
    Main {
        Quit "quit" ["q"]
        Help "help" ["?"]
        NextPanel "next_panel" ["Tab"]
        PrevPanel "prev_panel" ["shift+Tab"]
        PrevDay "prev_day" ["h", "Left"]
        NextDay "next_day" ["l", "Right"]
        PrevWeek "prev_week" ["H"]
        NextWeek "next_week" ["L"]
        PrevMonth "prev_month" ["["]
        NextMonth "next_month" ["]"]
        ScrollDown "scroll_down" ["J", "PageDown"]
        ScrollUp "scroll_up" ["K", "PageUp"]
        Down "down" ["j", "Down"]
        Up "up" ["k", "Up"]
        Today "today" ["t"]
        Goto "goto" ["g"]
        Details "details" ["Enter"]
        Add "add" ["a"]
        Edit "edit" ["e"]
        Delete "delete" ["d"]
        ToggleDone "toggle_done" ["x", "Space"]
        PriorityUp "priority_up" ["+"]
        PriorityDown "priority_down" ["-"]
        ToggleWeekView "toggle_week_view" ["V"]
        Calendars "calendars" ["c"]
        ToggleCompleted "toggle_completed" ["C"]
        Sync "sync" ["S"]
        Reload "reload" ["R"]
        Warnings "warnings" ["W"]
        LayoutMode "layout_mode" ["w"]
    }
    Layout {
        LayoutDone "done" ["Esc", "w", "q"]
        LayoutNextPanel "next_panel" ["Tab"]
        LayoutPrevPanel "prev_panel" ["shift+Tab"]
        LayoutNextPreset "next_preset" ["Right", "n"]
        LayoutPrevPreset "prev_preset" ["Left", "p"]
        LayoutGrow "grow" ["+", "="]
        LayoutShrink "shrink" ["-"]
        LayoutGrowColumn "grow_column" [">"]
        LayoutShrinkColumn "shrink_column" ["<"]
        LayoutSwap "swap" ["s"]
        LayoutRotate "rotate" ["r"]
        LayoutHide "hide" ["x"]
        LayoutUnhide "unhide" ["u"]
        LayoutSave "save" ["Enter"]
    }
    Popup {
        PopupClose "close" ["Esc", "q"]
        PopupAccept "accept" ["Enter"]
        PopupDown "down" ["j", "Down"]
        PopupUp "up" ["k", "Up"]
        PopupToggle "toggle" ["Space", "Enter"]
        PopupYes "yes" ["y", "Y"]
        PopupNo "no" ["n", "N"]
        PopupThisOccurrence "this_occurrence" ["o", "O"]
        PopupAllOccurrences "all_occurrences" ["a", "A"]
    }
    Form {
        FormSave "save" ["Enter", "ctrl+s"]
        FormCancel "cancel" ["Esc"]
        FormNextField "next_field" ["Tab", "Down"]
        FormPrevField "prev_field" ["shift+Tab", "Up"]
        FormClear "clear" ["ctrl+u"]
        FormLineStart "line_start" ["ctrl+a"]
        FormLineEnd "line_end" ["ctrl+e"]
        FormNextCalendar "next_calendar" ["Right", "Space"]
        FormPrevCalendar "prev_calendar" ["Left"]
    }
}

/// Actions that are live at the same time, so must not share a key:
/// (where, actions). The main view, layout mode and form are one group
/// each; pop-ups are grouped by kind.
fn groups() -> Vec<(&'static str, Vec<Action>)> {
    use Action::*;
    let ctx = |c: Context| TABLE.iter().filter(|r| r.1 == c).map(|r| r.0).collect();
    vec![
        ("", ctx(Context::Main)),
        ("", ctx(Context::Layout)),
        ("", ctx(Context::Form)),
        ("help/details pop-up", vec![PopupClose, PopupAccept]),
        (
            "warnings pop-up",
            vec![PopupClose, PopupAccept, PopupDown, PopupUp],
        ),
        (
            "calendars pop-up",
            vec![PopupClose, PopupDown, PopupUp, PopupToggle],
        ),
        ("yes/no question", vec![PopupClose, PopupYes, PopupNo]),
        (
            "repeating-item question",
            vec![PopupClose, PopupThisOccurrence, PopupAllOccurrences],
        ),
    ]
}

impl Action {
    fn row(self) -> &'static (Action, Context, &'static str, &'static [&'static str]) {
        TABLE
            .iter()
            .find(|r| r.0 == self)
            .expect("every action is in TABLE")
    }

    pub fn context(self) -> Context {
        self.row().1
    }

    /// Name in the config, e.g. `next_day`.
    pub fn name(self) -> &'static str {
        self.row().2
    }

    /// Full config path, e.g. `keys.layout.save`.
    pub fn path(self) -> String {
        format!("{}.{}", self.context().path(), self.name())
    }

    /// All actions of a context, in help order.
    pub fn in_context(ctx: Context) -> impl Iterator<Item = Action> {
        TABLE.iter().filter(move |r| r.1 == ctx).map(|r| r.0)
    }
}

/// Keys for every action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    binds: BTreeMap<Action, Vec<KeyChord>>,
}

impl Default for Keymap {
    fn default() -> Keymap {
        let binds = TABLE
            .iter()
            .map(|(a, _, _, keys)| {
                let keys = keys
                    .iter()
                    .map(|k| KeyChord::parse(k).expect("default keys parse"))
                    .collect();
                (*a, keys)
            })
            .collect();
        Keymap { binds }
    }
}

impl Keymap {
    /// The defaults with the `[keys]` table applied. Errors name the key,
    /// e.g. `keys.layout.save: cannot parse key 'ctrl+'`.
    pub fn from_toml(v: &Value) -> Result<Keymap, String> {
        let mut map = Keymap::default();
        let Some(top) = v.as_table() else {
            return Err("keys: expected a table".into());
        };
        for (name, val) in top {
            if let Some(ctx) = Context::ALL.into_iter().find(|c| c.table() == Some(name)) {
                let Some(sub) = val.as_table() else {
                    return Err(format!("{}: expected a table", ctx.path()));
                };
                for (n, v) in sub {
                    map.set(ctx, n, v)?;
                }
            } else {
                map.set(Context::Main, name, val)?;
            }
        }
        Ok(map)
    }

    fn set(&mut self, ctx: Context, name: &str, v: &Value) -> Result<(), String> {
        let path = format!("{}.{name}", ctx.path());
        let action = Action::in_context(ctx)
            .find(|a| a.name() == name)
            .ok_or_else(|| {
                let known: Vec<&str> = Action::in_context(ctx).map(|a| a.name()).collect();
                format!("{path}: unknown action (known: {})", known.join(", "))
            })?;
        let specs: Vec<&str> = match v {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a
                .iter()
                .map(|x| x.as_str())
                .collect::<Option<_>>()
                .ok_or_else(|| format!("{path}: expected a key or a list of keys"))?,
            _ => return Err(format!("{path}: expected a key or a list of keys")),
        };
        let mut keys = Vec::new();
        for s in specs {
            let k = KeyChord::parse(s).map_err(|e| format!("{path}: {e}"))?;
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        self.binds.insert(action, keys);
        Ok(())
    }

    /// Keys bound to `a`, in the order given (the first is shown in hints).
    pub fn keys(&self, a: Action) -> &[KeyChord] {
        self.binds.get(&a).map_or(&[], |v| v.as_slice())
    }

    pub fn is(&self, a: Action, k: &KeyChord) -> bool {
        self.keys(a).contains(k)
    }

    /// The first action of `ctx` (in help order) bound to `k`.
    pub fn find(&self, ctx: Context, k: &KeyChord) -> Option<Action> {
        Action::in_context(ctx).find(|a| self.is(*a, k))
    }

    /// Keys bound to two actions that are live at the same time.
    pub fn conflicts(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (where_, actions) in groups() {
            for (i, a) in actions.iter().enumerate() {
                for b in &actions[i + 1..] {
                    for k in self.keys(*a) {
                        if self.is(*b, k) {
                            let place = if where_.is_empty() {
                                String::new()
                            } else {
                                format!(" (both apply in the {where_})")
                            };
                            out.push(format!(
                                "{}: '{}' is also bound to {}{place}",
                                a.path(),
                                k.spec(),
                                b.path()
                            ));
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn parses_keys() {
        assert_eq!(k("a"), KeyChord::plain(Key::Char('a')));
        assert_eq!(k("shift+a"), k("A"));
        assert_eq!(k("shift+tab"), KeyChord::plain(Key::BackTab));
        assert_eq!(k("ctrl+S"), k("ctrl+s"));
        assert_eq!(k("+"), KeyChord::plain(Key::Char('+')));
        assert_eq!(k("ctrl++").key, Key::Char('+'));
        assert_eq!(k("space"), KeyChord::plain(Key::Char(' ')));
        assert_eq!(k("PgDn"), KeyChord::plain(Key::PageDown));
        assert_eq!(k("f5"), KeyChord::plain(Key::F(5)));
        assert!(k("alt+Enter").alt);
        for bad in ["", "ctrl+", "hyper+a", "Enterr", "F0", "F99"] {
            assert!(KeyChord::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn spec_and_display() {
        for s in [
            "a", "A", "ctrl+s", "alt+x", "Space", "Left", "BackTab", "F12", "+", "shift+Up",
        ] {
            assert_eq!(k(&k(s).spec()), k(s), "{s}");
        }
        assert_eq!(k("ctrl+u").to_string(), "Ctrl-U");
        assert_eq!(k("shift+tab").to_string(), "Shift-Tab");
        assert_eq!(k("PageDown").to_string(), "PgDn");
        assert_eq!(k("Left").to_string(), "←");
    }

    #[test]
    fn defaults_have_no_conflicts() {
        let m = Keymap::default();
        assert!(m.conflicts().is_empty(), "{:#?}", m.conflicts());
        assert_eq!(m.find(Context::Main, &k("l")), Some(Action::NextDay));
        assert_eq!(m.find(Context::Layout, &k("l")), None);
    }

    #[test]
    fn overrides_replace_and_unbind() {
        let v: Value = toml::from_str(
            r#"
            next_day = "n"
            sync = []
            [layout]
            save = ["ctrl+s", "Enter"]
            "#,
        )
        .unwrap();
        let m = Keymap::from_toml(&v).unwrap();
        assert_eq!(m.keys(Action::NextDay), [k("n")]);
        assert!(m.keys(Action::Sync).is_empty());
        assert_eq!(m.keys(Action::LayoutSave), [k("ctrl+s"), k("Enter")]);
        assert_eq!(m.keys(Action::PrevDay), [k("h"), k("Left")]);
    }

    #[test]
    fn errors_name_the_key() {
        let err = |s: &str| Keymap::from_toml(&toml::from_str(s).unwrap()).unwrap_err();
        assert!(err("nxt_day = \"n\"").starts_with("keys.nxt_day: unknown action"));
        assert!(err("[form]\nsave = \"ctrl+\"").starts_with("keys.form.save: cannot parse"));
        assert!(err("quit = 1").contains("keys.quit: expected a key"));
        assert!(err("layout = 1").starts_with("keys.layout: expected a table"));
    }

    #[test]
    fn conflicts_are_reported() {
        let v: Value = toml::from_str("next_day = \"j\"\n[popup]\nyes = \"Esc\"").unwrap();
        let c = Keymap::from_toml(&v).unwrap().conflicts();
        assert_eq!(c.len(), 2, "{c:#?}");
        assert_eq!(c[0], "keys.next_day: 'j' is also bound to keys.down");
        assert!(c[1].contains("keys.popup.close: 'Esc' is also bound to keys.popup.yes"));
    }
}
