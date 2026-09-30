//! Human-readable `wn ask` output: one line per hint, path first, score last. The text is always
//! built with ANSI styles; [`finish`] strips them unless color is on (see [`color_enabled`]), so
//! pipes and agents without `--json` get plain text.

use anstyle::{AnsiColor, Style};
use wn_core::rank::{AnswerState, Hint, Outcome};

/// `--color`: when to style the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ColorWhen {
    // Color when stdout is a terminal, honoring `NO_COLOR` and `CLICOLOR_FORCE`.
    #[default]
    Auto,
    Always,
    Never,
}

/// Whether stdout gets colors: `--color` first, then `NO_COLOR`, `CLICOLOR_FORCE`, `CLICOLOR`
/// and whether stdout is a terminal.
pub fn color_enabled(when: ColorWhen) -> bool {
    match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            anstream::AutoStream::choice(&std::io::stdout()) != anstream::ColorChoice::Never
        }
    }
}

/// Styled text as is when `color`, else with every escape code removed.
pub fn finish(text: String, color: bool) -> String {
    if color {
        text
    } else {
        anstream::adapter::strip_str(&text).to_string()
    }
}

const DIM: Style = Style::new().dimmed();
const BOLD: Style = Style::new().bold();
const WARN: Style = AnsiColor::Yellow.on_default();
const LOC: Style = AnsiColor::Cyan.on_default();
const EXACT: Style = AnsiColor::Green.on_default();
const CONFIG: Style = AnsiColor::Magenta.on_default();

fn paint(style: Style, text: &str) -> String {
    format!("{style}{text}{style:#}")
}

/// One row's left side: styled text and its visible width.
struct Row {
    styled: String,
    width: usize,
    score: f64,
}

fn path_row(h: &Hint) -> (String, usize) {
    let (dir, file) = h.path.rsplit_once('/').map_or(("", h.path.as_str()), |(d, f)| (d, f));
    let dir = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    (
        format!("{}{}", paint(DIM, &dir), paint(BOLD, file)),
        h.path.chars().count(),
    )
}

fn rows(out: &Outcome) -> Vec<Row> {
    let mut rows = Vec::new();
    for h in &out.hints.files {
        let (mut styled, mut width) = path_row(h);
        if h.evidence.as_deref() == Some("exact") {
            styled.push_str(&format!("  {}", paint(EXACT, "(exact)")));
            width += 9;
        }
        rows.push(Row {
            styled,
            width,
            score: h.similarity,
        });
    }
    for h in &out.hints.functions {
        let (mut styled, mut width) = path_row(h);
        let tail = format!(":{}  {}", h.line.unwrap_or(0), h.name.as_deref().unwrap_or(""));
        styled.push_str(&paint(LOC, &tail));
        width += tail.chars().count();
        rows.push(Row {
            styled,
            width,
            score: h.similarity,
        });
    }
    for h in &out.hints.configs {
        let (mut styled, width) = path_row(h);
        styled.push_str(&format!("  {}", paint(CONFIG, "(config)")));
        rows.push(Row {
            styled,
            width: width + 10,
            score: h.similarity,
        });
    }
    rows
}

/// `wn ask` text (styled): hints, or a one-line abstain / fail-open message, then `note`.
pub fn render(out: &Outcome, note: Option<&str>) -> String {
    let mut lines = Vec::new();
    match out.state {
        AnswerState::Ok => {
            let rows = rows(out);
            let width = rows.iter().map(|r| r.width).max().unwrap_or(0);
            for r in rows {
                let pad = " ".repeat(width - r.width + 2);
                let score = paint(DIM, &format!("{:.2}", r.score));
                lines.push(format!("{}{pad}{score}", r.styled));
            }
        }
        AnswerState::Abstain => {
            let reason = out.abstain.as_deref().unwrap_or("");
            let reason = if reason.is_empty() {
                String::new()
            } else {
                format!(" {}", paint(DIM, &format!("({reason})")))
            };
            lines.push(format!(
                "{}{reason}; try rg for exact names, or add detail",
                paint(WARN, "no confident hint")
            ));
        }
        other => {
            let detail = out.error.as_deref().filter(|d| !d.is_empty());
            let text = match (other, detail) {
                (AnswerState::EmptyIndex, _) => {
                    "nothing to rank: no source files found here; run wn inside a repository (see `wn status`)".to_string()
                }
                (AnswerState::UnsupportedScope, _) => {
                    "no supported source files here; use normal search".to_string()
                }
                (AnswerState::StaleIndex, _) => {
                    "the index is out of date (run `wn init`); use normal search".to_string()
                }
                (_, Some(d)) => format!("wn could not answer ({d}); use normal search"),
                (_, None) => "wn could not answer; use normal search".to_string(),
            };
            lines.push(paint(WARN, &text));
        }
    }
    if let Some(note) = note {
        lines.push(format!("{} {note}", paint(WARN, "note:")));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wn_core::rank::Hints;

    fn hint(path: &str, similarity: f64) -> Hint {
        Hint {
            path: path.into(),
            similarity,
            evidence: None,
            name: None,
            line: None,
        }
    }

    fn sample() -> Outcome {
        let mut exact = hint("src/auth/session.py", 0.62);
        exact.evidence = Some("exact".into());
        let mut func = hint("src/upload.go", 0.41);
        func.name = Some("retryUpload".into());
        func.line = Some(42);
        Outcome {
            hints: Hints {
                files: vec![exact, hint("main.rs", 0.5)],
                functions: vec![func],
                configs: vec![hint("deploy/app.yaml", 0.3)],
            },
            ..Outcome::default()
        }
    }

    #[test]
    fn plain_hints_are_one_aligned_line_each_path_first() {
        let text = finish(render(&sample(), None), false);
        assert_eq!(
            text,
            "\
src/auth/session.py  (exact)   0.62
main.rs                        0.50
src/upload.go:42  retryUpload  0.41
deploy/app.yaml  (config)      0.30"
        );
    }

    #[test]
    fn colored_text_strips_to_the_plain_text() {
        let styled = render(&sample(), Some("x"));
        assert!(styled.contains('\x1b'));
        let plain = finish(styled, false);
        assert!(!plain.contains('\x1b'));
        assert!(plain.ends_with("\nnote: x"));
    }

    #[test]
    fn abstain_and_fail_open_are_single_lines() {
        let abstain = Outcome {
            state: AnswerState::Abstain,
            abstain: Some("top similarity 0.10 < 0.30".into()),
            ..Outcome::default()
        };
        assert_eq!(
            finish(render(&abstain, None), false),
            "no confident hint (top similarity 0.10 < 0.30); try rg for exact names, or add detail"
        );
        for state in [
            AnswerState::EmptyIndex,
            AnswerState::UnsupportedScope,
            AnswerState::StaleIndex,
            AnswerState::Error,
        ] {
            let out = Outcome {
                state,
                ..Outcome::default()
            };
            let text = finish(render(&out, None), false);
            assert_eq!(text.lines().count(), 1, "{text}");
        }
    }
}
