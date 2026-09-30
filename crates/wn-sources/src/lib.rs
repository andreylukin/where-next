//! Source plugins: which files where-next indexes, and the text each one is embedded as.
//!
//! This is a line-for-line port of the reference implementation the published model was trained
//! and evaluated with. Document text must be built exactly the same way at serving time, so the
//! behaviour here is pinned by golden tests (`tests/golden.rs`) rather than improved freely:
//!
//! * a **source file** is embedded as `"file: " + skeleton(path, text)` — the path, the first
//!   meaningful doc/comment line and the names of its definitions;
//! * a **function** is embedded as `"function: path::name"` plus the file's doc line;
//! * a **config file** (Dockerfile, YAML, TOML, …) is embedded as its first non-comment lines and
//!   ranked separately from code.

use std::io;
use std::path::Path;
use std::sync::OnceLock;

use fancy_regex::Regex;

/// Maximum bytes read from a source file (larger files are truncated, as in the reference).
pub const MAX_SOURCE_BYTES: usize = 400_000;
/// Maximum bytes read from a config file.
pub const MAX_CONFIG_BYTES: usize = 200_000;
/// Maximum characters of a file skeleton.
pub const SKELETON_LIMIT: usize = 1500;
/// Non-comment lines kept from a config file.
pub const CONFIG_LINES: usize = 15;

/// Exact fragments worth checking in indexed source files before vector ranking.
pub fn query_literals(text: &str) -> Vec<String> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| Regex::new(r#"[\"'`]([^\"'`\n]{8,120})[\"'`]|\b([A-Z][A-Za-z0-9]*[A-Z][A-Za-z0-9]*|[a-z][a-z0-9]*(?:_[a-z0-9]+)+|[A-Za-z0-9_./-]+\.[A-Za-z0-9]+:\d+)\b"#).expect("literal regex"));
    let mut out = Vec::new();
    for hit in pattern.captures_iter(text).flatten() {
        if let Some(m) = hit.get(1).or_else(|| hit.get(2)) {
            let literal = m.as_str().split(':').next().unwrap_or("").trim();
            if literal.len() >= 4 && !out.iter().any(|s| s == literal) {
                out.push(literal.to_string());
            }
        }
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some((prefix, rest)) = trimmed.split_once(':') {
            if prefix.contains("panicked")
                || prefix.contains("Error")
                || prefix.contains("Exception")
                || prefix.eq_ignore_ascii_case("panic")
            {
                let phrase = rest
                    .trim()
                    .split(['.', '\n', '\'', '"', '`'])
                    .next()
                    .unwrap_or("")
                    .trim();
                if phrase.len() >= 12 && phrase.len() <= 120 && !out.iter().any(|s| s == phrase) {
                    out.push(phrase.to_string());
                }
            }
        }
    }
    out.truncate(12);
    out
}

#[cfg(test)]
mod literal_tests {
    use super::query_literals;

    #[test]
    fn extracts_error_phrases_symbols_and_file_references() {
        let hits = query_literals("panic: handlers are already registered for path ... at tree.go:243; FastAPIError; ShouldBindJSON; `not awaited`");
        assert!(hits.contains(&"handlers are already registered for path".to_string()));
        assert!(hits.contains(&"tree.go".to_string()));
        assert!(hits.contains(&"FastAPIError".to_string()));
        assert!(hits.contains(&"ShouldBindJSON".to_string()));
        assert!(hits.contains(&"not awaited".to_string()));
        let gin = query_literals("panic: handlers are already registered for path '/users/:id'");
        assert!(gin.contains(&"handlers are already registered for path".to_string()));
        let axum = query_literals("thread 'main' panicked: Overlapping method route. Handler for `GET /users` already exists");
        assert!(axum.contains(&"Overlapping method route".to_string()));
    }
}

const LANGS: &[(&str, &str)] = &[
    (".py", "python"),
    (".go", "go"),
    (".ts", "typescript"),
    (".tsx", "typescript"),
    (".js", "javascript"),
    (".jsx", "javascript"),
    (".mjs", "javascript"),
    (".java", "java"),
    (".kt", "kotlin"),
    (".kts", "kotlin"),
    (".rs", "rust"),
    (".c", "c"),
    (".h", "c"),
    (".cc", "cpp"),
    (".cpp", "cpp"),
    (".cxx", "cpp"),
    (".hpp", "cpp"),
    (".hh", "cpp"),
    (".rb", "ruby"),
    (".php", "php"),
    (".sh", "shell"),
    (".bash", "shell"),
    (".swift", "swift"),
    (".scala", "scala"),
    (".cs", "csharp"),
    (".lua", "lua"),
    (".ex", "elixir"),
    (".exs", "elixir"),
];

const CONFIG_NAMES: &[&str] = &[
    "makefile",
    "dockerfile",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "cargo.toml",
    "setup.cfg",
    "setup.py",
    "tsconfig.json",
    "procfile",
    "justfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    ".env.example",
    "requirements.txt",
];

const KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "return",
    "catch",
    "else",
    "do",
    "try",
    "new",
    "delete",
    "sizeof",
    "function",
    "constructor",
    "super",
    "this",
    "typeof",
    "await",
    "yield",
    "case",
    "throw",
];

const CODE_START: &[&str] = &[
    "package ",
    "import ",
    "from ",
    "use ",
    "#include",
    "'use strict'",
    "\"use strict\"",
    "<?php",
];

/// What kind of indexable file a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Code in a supported language: what the model was trained on.
    Source,
    /// Build and deployment configuration, ranked separately from code.
    Config,
}

impl Kind {
    /// Stable lowercase name, as used in the index and in tool output.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Source => "source",
            Kind::Config => "config",
        }
    }
}

/// A definition found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The defined name (function, method, class, type, module, …).
    pub name: String,
    /// 1-based line where the definition starts.
    pub line: usize,
    /// Last line of the definition: the line before the next definition at the same or lower
    /// indentation, or the last line of the file.
    pub end: usize,
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("built-in pattern compiles")
}

fn skip_path() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        re(r"(^|/)(vendor|node_modules|third_party|thirdparty|dist|build|\.git|__generated__|generated|gen)/|\.min\.js$|_pb2\.py$|\.pb\.go$|\.pb\.(cc|h)$|\.d\.ts$")
    })
}

fn skip_index() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        re(r"(^|/)(vendor|node_modules|third_party|thirdparty|dist|build|\.git|generated|gen)/|(^|/)[^/]*lock[^/]*$|\.lock$")
    })
}

fn config_ext() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"(?i)\.(ya?ml|toml|ini|cfg|conf|properties|service|timer|nginx)$"))
}

fn comment() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        re(r##"^\s*(?:#(?!!)|//+|/\*+|\*|"""|'''|--|;+)\s*(?P<text>.*?)\s*(?:\*/|"""|''')?\s*$"##)
    })
}

fn boiler() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        re(r"(?i)copyright|license|licence|spdx|all rights reserved|^-\*-|coding[:=]|eslint|prettier|@ts-|nolint|go:build|\+build|pylint|noqa|^!|governed by|use of this source|permission is hereby|warranty|licensed under")
    })
}

const TS_ARROW: &str = r"^[ \t]*(?:export[ \t]+)?(?:const|let|var)[ \t]+(?P<name>\w+)[ \t]*(?::[^=]+)?=[ \t]*(?:async[ \t]*)?(?:\([^)]*\)|\w+)[ \t]*(?::[^=]+)?=>";
const TS_METHOD: &str = r"^[ \t]+(?:(?:public|private|protected|static|async|readonly|override)[ \t]+)*(?P<name>\w+)[ \t]*(?:<[^>]*>)?\([^)]*\)[ \t]*(?::[^{]+)?\{[ \t]*$";
const JAVA_TYPE: &str = r"^[ \t]*(?:(?:public|private|protected|static|final|abstract|sealed)[ \t]+)*(?:class|interface|enum|record)[ \t]+(?P<name>\w+)";
const JAVA_METHOD: &str = r"^[ \t]*(?:(?:public|private|protected|static|final|synchronized|abstract|native|default)[ \t]+)+[\w<>\[\],.? \t]+[ \t]+(?P<name>\w+)[ \t]*\(";
const C_FUNC: &str = r"^(?!(?:if|for|while|switch|return|else|do)\b)[A-Za-z_][\w \t\*]*?[ \t\*](?P<name>\w+)[ \t]*\([^;]*$";
const CPP_FUNC: &str = r"^(?!(?:if|for|while|switch|return|else|do)\b)[A-Za-z_][\w \t\*&:<>,]*?[ \t\*&](?P<name>[\w:~]+)[ \t]*\([^;]*$";
const CS_TYPE: &str = r"^[ \t]*(?:(?:public|private|protected|internal|static|partial|abstract|sealed)[ \t]+)*(?:class|interface|struct|enum|record)[ \t]+(?P<name>\w+)";
const CS_METHOD: &str = r"^[ \t]*(?:(?:public|private|protected|internal|static|virtual|override|async|abstract)[ \t]+)+[\w<>\[\],.? \t]+[ \t]+(?P<name>\w+)[ \t]*\(";

fn raw_patterns(lang: &str) -> Vec<&'static str> {
    match lang {
        "python" => vec![r"^[ \t]*(?:async[ \t]+)?(?:def|class)[ \t]+(?P<name>\w+)"],
        "go" => vec![
            r"^func[ \t]+(?:\([^)]*\)[ \t]*)?(?P<name>\w+)",
            r"^type[ \t]+(?P<name>\w+)[ \t]+(?:struct|interface)",
        ],
        "typescript" | "javascript" => vec![
            r"^[ \t]*(?:export[ \t]+)?(?:default[ \t]+)?(?:async[ \t]+)?function\*?[ \t]*(?P<name>\w+)",
            r"^[ \t]*(?:export[ \t]+)?(?:default[ \t]+)?(?:abstract[ \t]+)?(?:class|interface|enum)[ \t]+(?P<name>\w+)",
            TS_ARROW,
            TS_METHOD,
        ],
        "java" => vec![JAVA_TYPE, JAVA_METHOD],
        "kotlin" => vec![
            r"^[ \t]*(?:\w+[ \t]+)*(?:class|interface|object)[ \t]+(?P<name>\w+)",
            r"^[ \t]*(?:\w+[ \t]+)*fun[ \t]+(?:<[^>]+>[ \t]*)?(?:[\w.]+\.)?(?P<name>\w+)[ \t]*\(",
        ],
        "rust" => vec![
            r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:const[ \t]+)?(?:async[ \t]+)?(?:unsafe[ \t]+)?fn[ \t]+(?P<name>\w+)",
            r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:struct|enum|trait|mod)[ \t]+(?P<name>\w+)",
        ],
        "c" => vec![
            C_FUNC,
            r"^(?:typedef[ \t]+)?struct[ \t]+(?P<name>\w+)[ \t]*\{",
        ],
        "cpp" => vec![
            CPP_FUNC,
            r"^[ \t]*(?:class|struct|namespace)[ \t]+(?P<name>\w+)[^;]*$",
        ],
        "ruby" => vec![
            r"^[ \t]*def[ \t]+(?:self\.)?(?P<name>\w+[?!=]?)",
            r"^[ \t]*(?:class|module)[ \t]+(?P<name>[\w:]+)",
        ],
        "php" => vec![
            r"^[ \t]*(?:(?:public|private|protected|static|abstract|final)[ \t]+)*function[ \t]+&?(?P<name>\w+)",
            r"^[ \t]*(?:(?:abstract|final)[ \t]+)?(?:class|interface|trait|enum)[ \t]+(?P<name>\w+)",
        ],
        "shell" => vec![
            r"^[ \t]*function[ \t]+(?P<name>[\w-]+)",
            r"^[ \t]*(?P<name>[\w-]+)[ \t]*\(\)[ \t]*\{?",
        ],
        "swift" => vec![
            r"^[ \t]*(?:\w+[ \t]+)*func[ \t]+(?P<name>\w+)",
            r"^[ \t]*(?:\w+[ \t]+)*(?:class|struct|enum|protocol|extension)[ \t]+(?P<name>\w+)",
        ],
        "scala" => vec![
            r"^[ \t]*(?:\w+[ \t]+)*def[ \t]+(?P<name>\w+)",
            r"^[ \t]*(?:\w+[ \t]+)*(?:class|object|trait)[ \t]+(?P<name>\w+)",
        ],
        "csharp" => vec![CS_TYPE, CS_METHOD],
        "lua" => vec![r"^[ \t]*(?:local[ \t]+)?function[ \t]+(?P<name>[\w.:]+)"],
        "elixir" => vec![
            r"^[ \t]*defp?[ \t]+(?P<name>\w+[?!]?)",
            r"^[ \t]*defmodule[ \t]+(?P<name>[\w.]+)",
        ],
        _ => vec![],
    }
}

fn patterns(lang: &str) -> &'static [Regex] {
    static CACHE: OnceLock<Vec<(&'static str, Vec<Regex>)>> = OnceLock::new();
    let all = CACHE.get_or_init(|| {
        let mut langs: Vec<&'static str> = LANGS.iter().map(|(_, l)| *l).collect();
        langs.sort_unstable();
        langs.dedup();
        langs
            .into_iter()
            .map(|l| {
                let compiled = raw_patterns(l)
                    .into_iter()
                    .map(|p| re(&format!("(?m){p}")))
                    .collect();
                (l, compiled)
            })
            .collect()
    });
    all.iter()
        .find(|(l, _)| *l == lang)
        .map(|(_, ps)| ps.as_slice())
        .unwrap_or(&[])
}

/// Whitespace as Python's `str.isspace` defines it (Unicode whitespace plus `\x1c`–`\x1f`).
pub fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with Python's definition of whitespace.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// The first `n` characters of `s` (Python `s[:n]`).
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Lines split the way Python's `str.splitlines` splits them.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if is_break {
            out.push(&text[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = chars.peek() {
                    chars.next();
                    next = j + 1;
                }
            }
            start = next;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// The language of a path from its extension, or `None` if unsupported.
pub fn lang_of(path: &str) -> Option<&'static str> {
    let dot = path.rfind('.')?;
    let ext = path[dot..].to_lowercase();
    LANGS.iter().find(|(e, _)| *e == ext).map(|(_, l)| *l)
}

fn is_match(r: &Regex, s: &str) -> bool {
    r.is_match(s).unwrap_or(false)
}

/// Whether a path is source code the model understands (supported language, not vendored or
/// generated).
pub fn is_source(path: &str) -> bool {
    lang_of(path).is_some() && !is_match(skip_path(), path)
}

/// Whether a path should be skipped entirely (vendored, generated, build output, lockfiles).
pub fn is_skipped(path: &str) -> bool {
    is_match(skip_index(), path)
}

/// How a path is indexed: as source, as config, or not at all.
pub fn kind_of(path: &str) -> Option<Kind> {
    if is_skipped(path) {
        return None;
    }
    if is_source(path) {
        return Some(Kind::Source);
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    if CONFIG_NAMES.contains(&name.as_str())
        || is_match(config_ext(), &name)
        || name.starts_with("requirements")
    {
        return Some(Kind::Config);
    }
    None
}

/// Definitions in `text`, in file order.
pub fn symbols(path: &str, text: &str) -> Vec<Symbol> {
    let Some(lang) = lang_of(path) else {
        return Vec::new();
    };
    // line -> (name, indentation); the first pattern (and first match) to claim a line wins.
    let mut found: std::collections::BTreeMap<usize, (String, usize)> = Default::default();
    for pattern in patterns(lang) {
        for caps in pattern.captures_iter(text).flatten() {
            let Some(name) = caps.name("name").map(|m| m.as_str()) else {
                continue;
            };
            if name.is_empty() || KEYWORDS.contains(&name) {
                continue;
            }
            let whole = caps.get(0).expect("group 0 exists");
            let line = text[..whole.start()].matches('\n').count() + 1;
            let matched = whole.as_str();
            let indent =
                matched.chars().count() - matched.trim_start_matches(py_space).chars().count();
            found
                .entry(line)
                .or_insert_with(|| (name.to_string(), indent));
        }
    }
    let rows: Vec<(usize, String, usize)> = found
        .into_iter()
        .map(|(line, (name, indent))| (line, name, indent))
        .collect();
    let total = text.matches('\n').count() + 1;
    rows.iter()
        .enumerate()
        .map(|(i, (line, name, indent))| {
            let end = rows[i + 1..]
                .iter()
                .find(|(_, _, later_indent)| later_indent <= indent)
                .map(|(later_line, _, _)| later_line - 1)
                .unwrap_or(total);
            Symbol {
                name: name.clone(),
                line: *line,
                end,
            }
        })
        .collect()
}

/// The first meaningful doc/comment line near the top of a file, skipping shebangs and license
/// headers. `None` when code starts before any comment.
pub fn first_comment(text: &str) -> Option<String> {
    let head = take_chars(text, 3000);
    for raw in split_lines(head).into_iter().take(40) {
        let caps = comment().captures(raw).ok().flatten();
        let Some(caps) = caps else {
            let stripped = py_strip(raw);
            if !stripped.is_empty() && !CODE_START.iter().any(|p| stripped.starts_with(p)) {
                return None;
            }
            continue;
        };
        let body = caps
            .name("text")
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim_matches(|c| matches!(c, ' ' | '*' | '-' | '=' | '/'));
        if body.chars().count() >= 12 && !is_match(boiler(), body) {
            return Some(take_chars(body, 160).to_string());
        }
    }
    None
}

/// Path, first doc/comment line and definition names, capped at `limit` characters.
pub fn skeleton(path: &str, text: &str, limit: usize) -> String {
    let mut head = path.to_string();
    if let Some(doc) = first_comment(text) {
        head.push('\n');
        head.push_str(&doc);
    }
    let mut names: Vec<String> = Vec::new();
    for s in symbols(path, text) {
        if !names.contains(&s.name) {
            names.push(s.name);
        }
    }
    let full = format!("{head}\n{}", names.join(" "));
    take_chars(&full, limit).to_string()
}

/// Embedding text for a source file.
pub fn file_doc(path: &str, text: &str) -> String {
    format!("file: {}", skeleton(path, text, SKELETON_LIMIT))
}

/// Embedding text for a function, given the file's doc line.
pub fn function_text(path: &str, name: &str, doc: Option<&str>) -> String {
    match doc {
        Some(d) => format!("function: {path}::{name}\n{d}"),
        None => format!("function: {path}::{name}"),
    }
}

/// `(name, 1-based line, embedding text)` for each definition in a file.
pub fn function_docs(path: &str, text: &str) -> Vec<(String, usize, String)> {
    let doc = first_comment(text);
    symbols(path, text)
        .into_iter()
        .map(|s| {
            let t = function_text(path, &s.name, doc.as_deref());
            (s.name, s.line, t)
        })
        .collect()
}

/// Embedding text for a config file: its first `lines` non-empty, non-comment lines.
pub fn config_doc(path: &str, text: &str, lines: usize) -> String {
    let body: Vec<&str> = split_lines(text)
        .into_iter()
        .map(py_strip)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//"))
        .take(lines)
        .collect();
    let full = format!("file: {path}\n{}", body.join("\n"));
    take_chars(&full, 1500).to_string()
}

/// Read up to `max_bytes` of a file as text, replacing invalid UTF-8 like the reference does.
pub fn read_text(path: &Path, max_bytes: usize) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let cut = &bytes[..bytes.len().min(max_bytes)];
    Ok(String::from_utf8_lossy(cut).into_owned())
}
