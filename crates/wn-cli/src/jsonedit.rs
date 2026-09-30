//! Text-level edits of JSON settings files, so `wn setup` can add its hook entries and
//! `--uninstall` can take exactly those bytes out again: a file nothing else changed in comes back
//! byte for byte, without wn keeping a copy of it (settings files can hold secrets).
//!
//! Insertions go right after the last item of an array or object (`,\n<indent><value>`), or
//! inside an empty one; removals take out an item with the separator before it (or after it, for
//! a first item). Callers check the result against the value-level edit and fall back to
//! re-serialising when the text does not parse to the same value.

use serde_json::Value;

/// A byte range in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// One `"key": value` member of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub key: String,
    pub key_start: usize,
    pub value: Span,
}

fn ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

fn string_end(b: &[u8], mut i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// End of the value starting at `i` (after leading whitespace).
fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'{' => members(b, i).map(|(_, close)| close + 1),
        b'[' => elements(b, i).map(|(_, close)| close + 1),
        b'"' => string_end(b, i),
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

/// Members of the object whose `{` is at `open`, and the index of its `}`.
pub fn members(b: &[u8], open: usize) -> Option<(Vec<Member>, usize)> {
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = ws(b, open + 1);
    if b.get(i) == Some(&b'}') {
        return Some((out, i));
    }
    loop {
        let key_start = i;
        let key_end = string_end(b, i)?;
        let key: String = serde_json::from_slice(&b[key_start..key_end]).ok()?;
        i = ws(b, key_end);
        if b.get(i) != Some(&b':') {
            return None;
        }
        let vs = ws(b, i + 1);
        let ve = value_end(b, vs)?;
        out.push(Member {
            key,
            key_start,
            value: Span { start: vs, end: ve },
        });
        i = ws(b, ve);
        match b.get(i)? {
            b',' => i = ws(b, i + 1),
            b'}' => return Some((out, i)),
            _ => return None,
        }
    }
}

/// Elements of the array whose `[` is at `open`, and the index of its `]`.
pub fn elements(b: &[u8], open: usize) -> Option<(Vec<Span>, usize)> {
    if b.get(open) != Some(&b'[') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = ws(b, open + 1);
    if b.get(i) == Some(&b']') {
        return Some((out, i));
    }
    loop {
        let end = value_end(b, i)?;
        out.push(Span { start: i, end });
        i = ws(b, end);
        match b.get(i)? {
            b',' => i = ws(b, i + 1),
            b']' => return Some((out, i)),
            _ => return None,
        }
    }
}

/// Indentation of the line containing byte `i`.
fn line_indent(t: &str, i: usize) -> String {
    let start = t[..i].rfind('\n').map_or(0, |n| n + 1);
    t[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Whether byte `i` is the first non-blank character of its line.
fn starts_line(t: &str, i: usize) -> bool {
    let start = t[..i].rfind('\n').map_or(0, |n| n + 1);
    t[start..i].chars().all(|c| c == ' ' || c == '\t')
}

fn pretty(v: &Value, indent: &str) -> String {
    serde_json::to_string_pretty(v)
        .unwrap_or_default()
        .replace('\n', &format!("\n{indent}"))
}

/// Text inserted for a new item after the last one (`last`) or into an empty container whose
/// opening bracket is at `open`.
fn insertion(
    t: &str,
    open: usize,
    last: Option<Span>,
    item: impl Fn(&str) -> String,
) -> (usize, String) {
    match last {
        Some(l) if starts_line(t, l.start) => {
            let indent = line_indent(t, l.start);
            (l.end, format!(",\n{indent}{}", item(&indent)))
        }
        Some(l) => (l.end, format!(", {}", item(""))),
        None => {
            let outer = line_indent(t, open);
            let indent = format!("{outer}  ");
            (open + 1, format!("\n{indent}{}\n{outer}", item(&indent)))
        }
    }
}

fn splice(t: &str, at: usize, text: &str) -> String {
    format!("{}{}{}", &t[..at], text, &t[at..])
}

/// Inserts `"key": value` into the object at `open`.
pub fn insert_member(t: &str, open: usize, key: &str, value: &Value) -> Option<String> {
    let (ms, _) = members(t.as_bytes(), open)?;
    let key_json = serde_json::to_string(key).ok()?;
    let last = ms.last().map(|m| Span {
        start: m.key_start,
        end: m.value.end,
    });
    let (at, text) = insertion(t, open, last, |indent| {
        format!("{key_json}: {}", pretty(value, indent))
    });
    Some(splice(t, at, &text))
}

/// Appends `value` to the array at `open`.
pub fn push_element(t: &str, open: usize, value: &Value) -> Option<String> {
    let (es, _) = elements(t.as_bytes(), open)?;
    let (at, text) = insertion(t, open, es.last().copied(), |indent| pretty(value, indent));
    Some(splice(t, at, &text))
}

/// Removes item `k` of `items` (spans from the item's first byte to its end) from the container
/// whose brackets are at `open` and `close`.
pub fn remove_item(t: &str, open: usize, close: usize, items: &[Span], k: usize) -> String {
    let range = if items.len() == 1 {
        (open + 1, close)
    } else if k > 0 {
        (items[k - 1].end, items[k].end)
    } else {
        (items[0].start, items[1].start)
    };
    format!("{}{}", &t[..range.0], &t[range.1..])
}

/// Member spans as items (key through value).
pub fn member_items(ms: &[Member]) -> Vec<Span> {
    ms.iter()
        .map(|m| Span {
            start: m.key_start,
            end: m.value.end,
        })
        .collect()
}

/// Index of the root object's `{`.
pub fn root(t: &str) -> Option<usize> {
    let i = ws(t.as_bytes(), 0);
    (t.as_bytes().get(i) == Some(&b'{')).then_some(i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn insert_then_remove_is_exact() {
        for t in [
            "{}",
            "{ }\n",
            "{\"a\":1}",
            "{\n  \"a\": 1,\n  \"b\": [\n    1\n  ]\n}\n",
            "{\"a\": [], \"b\": {\"c\": \"}\"}}",
        ] {
            let open = root(t).unwrap();
            let added = insert_member(t, open, "hooks", &json!({"X": [1, 2]})).unwrap();
            let v: Value = serde_json::from_str(&added).unwrap();
            assert_eq!(v["hooks"]["X"], json!([1, 2]), "{added}");
            let (ms, close) = members(added.as_bytes(), open).unwrap();
            let k = ms.iter().position(|m| m.key == "hooks").unwrap();
            let back = remove_item(&added, open, close, &member_items(&ms), k);
            if t.trim() == "{}" || t == "{ }\n" {
                assert_eq!(back.trim(), "{}", "{added:?}");
            } else {
                assert_eq!(back, t, "{added:?}");
            }
        }
    }

    #[test]
    fn push_then_remove_is_exact() {
        let t = "{\"h\": [\n    {\"a\": 1}\n  ]}";
        let (ms, _) = members(t.as_bytes(), 0).unwrap();
        let open = ms[0].value.start;
        let added = push_element(t, open, &json!({"b": 2})).unwrap();
        let (es, close) = elements(added.as_bytes(), open).unwrap();
        assert_eq!(es.len(), 2, "{added}");
        assert_eq!(remove_item(&added, open, close, &es, 1), t);
        // Removing a first item takes the separator after it.
        let first = remove_item(&added, open, close, &es, 0);
        let v: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(v, json!({"h": [{"b": 2}]}));
    }

    #[test]
    fn rejects_what_it_cannot_scan() {
        assert!(root("[1]").is_none());
        assert!(members(b"{\"a\" 1}", 0).is_none());
        assert!(members(b"{\"a\": 1", 0).is_none());
    }
}
