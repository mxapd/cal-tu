//! Lenient, lossless iCalendar (RFC 5545) content-line parser and serialiser.
//!
//! Every property and parameter is kept verbatim (name, params, raw value), so
//! a component that is parsed and serialised again keeps all data, including
//! properties this crate does not understand. Interpretation of values (dates,
//! text escaping) happens on top of this layer.

use std::fmt::Write as _;

/// A property parameter, e.g. `TZID=Europe/Stockholm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    /// Parameter values, without surrounding quotes.
    pub values: Vec<String>,
}

/// One content line, e.g. `DTSTART;TZID=Europe/Stockholm:20261116T081500`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// Upper-cased property name.
    pub name: String,
    pub params: Vec<Param>,
    /// Raw (still escaped) value.
    pub value: String,
}

impl Property {
    pub fn new(name: &str, value: &str) -> Self {
        Property {
            name: name.to_ascii_uppercase(),
            params: Vec::new(),
            value: value.to_string(),
        }
    }

    /// First value of the parameter `name` (case-insensitive).
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .and_then(|p| p.values.first())
            .map(String::as_str)
    }

    /// The value interpreted as TEXT (escapes resolved).
    pub fn text(&self) -> String {
        unescape_text(&self.value)
    }
}

/// A component such as `VCALENDAR`, `VEVENT`, `VTODO`, `VALARM`, `VTIMEZONE`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    /// Upper-cased component name.
    pub name: String,
    pub properties: Vec<Property>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn new(name: &str) -> Self {
        Component {
            name: name.to_ascii_uppercase(),
            ..Default::default()
        }
    }

    /// First property called `name`.
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// All properties called `name`.
    pub fn props<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> + 'a {
        self.properties
            .iter()
            .filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    /// Text value of the first property called `name`.
    pub fn text(&self, name: &str) -> Option<String> {
        self.prop(name).map(Property::text)
    }

    /// Direct children called `name`.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> + 'a {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    /// Serialise to iCalendar text with CRLF line endings and folding.
    pub fn to_ical(&self) -> String {
        let mut out = String::new();
        self.write_into(&mut out);
        out
    }

    fn write_into(&self, out: &mut String) {
        write_folded(out, &format!("BEGIN:{}", self.name));
        for p in &self.properties {
            let mut line = p.name.clone();
            for param in &p.params {
                line.push(';');
                line.push_str(&param.name);
                line.push('=');
                let vals: Vec<String> = param.values.iter().map(|v| quote_param(v)).collect();
                line.push_str(&vals.join(","));
            }
            line.push(':');
            line.push_str(&p.value);
            write_folded(out, &line);
        }
        for c in &self.children {
            c.write_into(out);
        }
        write_folded(out, &format!("END:{}", self.name));
    }
}

/// Problems found while parsing; parsing continues past them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseWarning {
    pub line: usize,
    pub message: String,
}

/// Result of parsing a file: the top-level components plus any warnings.
#[derive(Debug, Default)]
pub struct Parsed {
    pub components: Vec<Component>,
    pub warnings: Vec<ParseWarning>,
}

/// Parse iCalendar text leniently.
///
/// Accepts CRLF or LF line endings, folded lines, a UTF-8 BOM, malformed lines
/// (skipped with a warning), and unbalanced BEGIN/END (auto-closed with a
/// warning).
pub fn parse(input: &str) -> Parsed {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut parsed = Parsed::default();
    let mut stack: Vec<Component> = Vec::new();

    for (lineno, line) in unfold(input) {
        if line.trim().is_empty() {
            continue;
        }
        let prop = match parse_line(&line) {
            Some(p) => p,
            None => {
                parsed.warnings.push(ParseWarning {
                    line: lineno,
                    message: format!("malformed line skipped: {}", truncate(&line, 60)),
                });
                continue;
            }
        };

        if prop.name == "BEGIN" {
            stack.push(Component::new(prop.value.trim()));
        } else if prop.name == "END" {
            let name = prop.value.trim().to_ascii_uppercase();
            // Close until the matching BEGIN; tolerate missing ENDs.
            if !stack.iter().any(|c| c.name == name) {
                parsed.warnings.push(ParseWarning {
                    line: lineno,
                    message: format!("END:{name} without matching BEGIN"),
                });
                continue;
            }
            loop {
                let comp = stack.pop().expect("checked above");
                let done = comp.name == name;
                if !done {
                    parsed.warnings.push(ParseWarning {
                        line: lineno,
                        message: format!("missing END:{}", comp.name),
                    });
                }
                match stack.last_mut() {
                    Some(parent) => parent.children.push(comp),
                    None => parsed.components.push(comp),
                }
                if done {
                    break;
                }
            }
        } else if let Some(cur) = stack.last_mut() {
            cur.properties.push(prop);
        } else {
            parsed.warnings.push(ParseWarning {
                line: lineno,
                message: format!("property {} outside of any component", prop.name),
            });
        }
    }

    while let Some(comp) = stack.pop() {
        parsed.warnings.push(ParseWarning {
            line: 0,
            message: format!("missing END:{} at end of file", comp.name),
        });
        match stack.last_mut() {
            Some(parent) => parent.children.push(comp),
            None => parsed.components.push(comp),
        }
    }
    parsed
}

/// Unfold content lines. Returns (1-based line number of the first physical
/// line, logical line).
fn unfold(input: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for (i, raw) in input.split('\n').enumerate() {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if (raw.starts_with(' ') || raw.starts_with('\t')) && !out.is_empty() {
            out.last_mut().unwrap().1.push_str(&raw[1..]);
        } else {
            out.push((i + 1, raw.to_string()));
        }
    }
    out
}

/// Parse `NAME;P1=a,"b";P2=c:value`. Colons inside quoted params are allowed.
fn parse_line(line: &str) -> Option<Property> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b';' && bytes[i] != b':' {
        i += 1;
    }
    let name = line[..i].trim();
    if name.is_empty()
        || i >= bytes.len()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let mut params = Vec::new();
    while i < bytes.len() && bytes[i] == b';' {
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b';' && bytes[i] != b':' {
            i += 1;
        }
        let pname = line[start..i].trim().to_ascii_uppercase();
        let mut values = Vec::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            loop {
                if i < bytes.len() && bytes[i] == b'"' {
                    i += 1;
                    let vstart = i;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += 1;
                    }
                    values.push(line[vstart..i].to_string());
                    if i < bytes.len() {
                        i += 1; // closing quote
                    }
                } else {
                    let vstart = i;
                    while i < bytes.len() && !matches!(bytes[i], b',' | b';' | b':') {
                        i += 1;
                    }
                    values.push(line[vstart..i].to_string());
                }
                if i < bytes.len() && bytes[i] == b',' {
                    i += 1;
                    continue;
                }
                break;
            }
        }
        if !pname.is_empty() {
            params.push(Param {
                name: pname,
                values,
            });
        }
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    Some(Property {
        name: name.to_ascii_uppercase(),
        params,
        value: line[i + 1..].to_string(),
    })
}

fn quote_param(v: &str) -> String {
    if v.contains([':', ';', ',']) {
        format!("\"{}\"", v.replace('"', ""))
    } else {
        v.to_string()
    }
}

/// Write `line` folded at 75 octets (never splitting a UTF-8 character).
fn write_folded(out: &mut String, line: &str) {
    let mut width = 0;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += len;
    }
    out.push_str("\r\n");
}

/// Resolve TEXT escapes: `\n`, `\N`, `\,`, `\;`, `\\`.
pub fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Escape a string for use as a TEXT value.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    let mut t: String = s.chars().take(n).collect();
    if t.len() < s.len() {
        let _ = write!(t, "…");
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_folded_lines_and_params() {
        let src = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Long \r\n title\r\n\
                   DTSTART;TZID=\"Europe/Stockholm\":20261116T081500\r\n\
                   ATTENDEE;MEMBER=\"mailto:a@x\",\"mailto:b@x\";CN=Bob:mailto:bob@x\r\n\
                   END:VEVENT\r\nEND:VCALENDAR\r\n";
        let p = parse(src);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        let ev = &p.components[0].children[0];
        assert_eq!(ev.text("SUMMARY").unwrap(), "Long title");
        assert_eq!(
            ev.prop("DTSTART").unwrap().param("tzid"),
            Some("Europe/Stockholm")
        );
        let att = ev.prop("ATTENDEE").unwrap();
        assert_eq!(att.params[0].values, vec!["mailto:a@x", "mailto:b@x"]);
        assert_eq!(att.value, "mailto:bob@x");
    }

    #[test]
    fn tolerates_garbage_and_missing_end() {
        let src = "BEGIN:VCALENDAR\nthis is garbage\nBEGIN:VEVENT\nSUMMARY:x\nEND:VCALENDAR\n";
        let p = parse(src);
        assert_eq!(p.components.len(), 1);
        assert_eq!(p.components[0].children[0].text("SUMMARY").unwrap(), "x");
        assert_eq!(p.warnings.len(), 2);
    }

    #[test]
    fn roundtrip_is_lossless() {
        let src = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Uni\r\nBEGIN:VEVENT\r\n\
                   UID:1@x\r\nSUMMARY:Caf\u{e9}\\, bar\\nnext\r\nX-CUSTOM;FOO=\"a:b\":keep me\r\n\
                   DESCRIPTION:Ä very long description that will definitely need to be folded because it exceeds seventy five octets\r\n\
                   BEGIN:VALARM\r\nTRIGGER:-PT15M\r\nACTION:DISPLAY\r\nEND:VALARM\r\n\
                   END:VEVENT\r\nEND:VCALENDAR\r\n";
        let first = parse(src).components;
        let out = first[0].to_ical();
        assert!(out.lines().all(|l| l.trim_end_matches('\r').len() <= 75));
        let second = parse(&out).components;
        assert_eq!(first, second);
        assert_eq!(
            second[0].children[0].text("SUMMARY").unwrap(),
            "Caf\u{e9}, bar\nnext"
        );
    }
}
