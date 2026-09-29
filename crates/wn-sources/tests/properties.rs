//! Property tests: invariants that hold for any input, not just the golden fixtures.

use proptest::prelude::*;
use wn_sources::{config_doc, first_comment, skeleton, split_lines, symbols};

const EXTS: &[&str] = &[
    "py", "go", "ts", "js", "java", "kt", "rs", "c", "cpp", "rb", "php", "sh", "swift", "scala",
    "cs", "lua", "ex", "txt",
];

fn source_text() -> impl Strategy<Value = String> {
    // Code-like lines plus arbitrary unicode, so both the patterns and the fallbacks are exercised.
    let line = prop_oneof![
        Just("def run(self):".to_string()),
        Just("    async def go():".to_string()),
        Just("func (s *S) Handle(w int) {".to_string()),
        Just("pub fn parse(x: u8) -> u8 {".to_string()),
        Just("class Thing:".to_string()),
        Just("// A comment line that is long enough".to_string()),
        Just("#!/usr/bin/env bash".to_string()),
        Just("}".to_string()),
        "\\PC{0,40}",
    ];
    prop::collection::vec(line, 0..40).prop_map(|ls| ls.join("\n"))
}

proptest! {
    #[test]
    fn symbols_are_ordered_and_spans_are_valid(ext in prop::sample::select(EXTS), text in source_text()) {
        let path = format!("dir/file.{ext}");
        let syms = symbols(&path, &text);
        let total = text.matches('\n').count() + 1;
        for pair in syms.windows(2) {
            prop_assert!(pair[0].line < pair[1].line);
        }
        for s in &syms {
            prop_assert!(!s.name.is_empty());
            prop_assert!(s.line >= 1 && s.line <= total);
            prop_assert!(s.end >= s.line.saturating_sub(1) && s.end <= total);
        }
    }

    #[test]
    fn skeleton_respects_limit_and_starts_with_path(
        ext in prop::sample::select(EXTS), text in source_text(), limit in 1usize..2000
    ) {
        let path = format!("src/mod.{ext}");
        let s = skeleton(&path, &text, limit);
        prop_assert!(s.chars().count() <= limit);
        let prefix: String = path.chars().take(limit).collect();
        prop_assert!(s.starts_with(&prefix));
    }

    #[test]
    fn first_comment_is_bounded(text in "\\PC{0,4000}") {
        if let Some(c) = first_comment(&text) {
            prop_assert!(c.chars().count() >= 12 && c.chars().count() <= 160);
        }
    }

    #[test]
    fn config_doc_is_bounded(text in "\\PC{0,3000}") {
        prop_assert!(config_doc("x.yaml", &text, 15).chars().count() <= 1500);
    }

    #[test]
    fn split_lines_roundtrips_plain_newlines(lines in prop::collection::vec("[^\\n\\r\\x0b\\x0c\\x1c-\\x1e\\x{85}\\x{2028}\\x{2029}]{0,20}", 0..20)) {
        let text = lines.join("\n");
        let got = split_lines(&text);
        if text.is_empty() {
            prop_assert!(got.is_empty());
        } else {
            let want: Vec<&str> = lines.iter().map(String::as_str).collect();
            // A trailing empty line disappears, as in Python.
            let want: Vec<&str> = if want.last() == Some(&"") && want.len() > 1 { want[..want.len() - 1].to_vec() } else { want };
            prop_assert_eq!(got, want);
        }
    }
}

#[test]
fn split_lines_matches_python_examples() {
    assert_eq!(split_lines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
    assert_eq!(split_lines("a\n\nb"), vec!["a", "", "b"]);
    assert_eq!(split_lines("\n"), vec![""]);
    assert!(split_lines("").is_empty());
    assert_eq!(split_lines("x\u{2028}y"), vec!["x", "y"]);
}
