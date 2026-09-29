//! Query text: exactly how the model was trained to read a task.
//!
//! The instruction prefix depends on what is being ranked; the recent context (errors, stack
//! traces, files just touched) is appended after the task and trimmed from the *oldest* end,
//! because the latest tool output carries the strongest signal.

use wn_sources::{py_strip, split_lines, take_chars};

/// Instruction for ranking files.
pub const INSTRUCT_FILE: &str = "Instruct: Given a coding task and recent context, find the file that must be read or edited next\nQuery: ";
/// Instruction for ranking functions.
pub const INSTRUCT_FUNCTION: &str = "Instruct: Given a coding task and recent context, find the function that must be edited\nQuery: ";
/// Characters of task + context kept before the instruction is prepended.
pub const QUERY_LIMIT: usize = 2400;
/// Token window the published model was trained and evaluated with.
pub const MAX_SEQ: usize = 384;

/// What a query ranks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Granularity {
    /// Whole files.
    File,
    /// Individual definitions.
    Function,
}

impl Granularity {
    fn instruction(self) -> &'static str {
        match self {
            Granularity::File => INSTRUCT_FILE,
            Granularity::Function => INSTRUCT_FUNCTION,
        }
    }
}

fn last_chars(s: &str, n: usize) -> &str {
    let total = s.chars().count();
    if total <= n {
        return s;
    }
    match s.char_indices().nth(total - n) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

/// The text embedded for a query: instruction, task, then the tail of the recent context.
pub fn query_text(query: &str, context: &str, granularity: Granularity) -> String {
    let query = py_strip(query);
    let context = py_strip(context);
    let limit = QUERY_LIMIT;
    let text = if context.is_empty() {
        take_chars(query, limit).to_string()
    } else {
        let qlen = query.chars().count();
        let clen = context.chars().count();
        let room = 400.max(limit - qlen.min(limit / 2));
        let head = take_chars(query, limit - room.min(clen));
        format!("{head}\nRecent context:\n{}", last_chars(context, room))
    };
    format!("{}{text}", granularity.instruction())
}

/// Characters kept by [`query_text_v2`] (models trained on it use a longer token window).
pub const QUERY_LIMIT_V2: usize = 3200;
/// Characters of the request kept by [`query_text_v2`].
pub const QUERY_REQUEST_CHARS_V2: usize = 900;

/// How a model expects its query laid out (`query_format` in `wn-model.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryFormat {
    /// [`query_text`]: task, then the tail of the whole context.
    #[default]
    V1,
    /// [`query_text_v2`]: fields ordered by value so truncation cuts the least useful part.
    V2,
}

/// The query text a model with `format` was trained on.
pub fn query_text_for(
    format: QueryFormat,
    query: &str,
    context: &str,
    granularity: Granularity,
) -> String {
    match format {
        QueryFormat::V1 => query_text(query, context, granularity),
        QueryFormat::V2 => query_text_v2(query, context, granularity),
    }
}

const LAST_OUTPUT_MARKERS: [&str; 2] = ["\nLast tool output:\n", "\nLast output:\n"];

/// Splits context into (earlier turns / files opened, last tool output), on the first
/// `Last tool output:` (or `Last output:`) line. Without a marker everything is earlier.
pub fn split_context(context: &str) -> (String, String) {
    let context = py_strip(context);
    let framed = format!("\n{context}");
    for marker in LAST_OUTPUT_MARKERS {
        if let Some(at) = framed.find(marker) {
            let head = &framed[..at];
            let last = &framed[at + marker.len()..];
            return (py_strip(head).to_string(), py_strip(last).to_string());
        }
    }
    (context.to_string(), String::new())
}

/// The v2 query layout: the request, then the tail of the last tool output (errors, stack
/// traces), then the newest earlier context, within [`QUERY_LIMIT_V2`] characters.
pub fn query_text_v2(query: &str, context: &str, granularity: Granularity) -> String {
    let query = take_chars(py_strip(query), QUERY_REQUEST_CHARS_V2);
    let (earlier, last) = split_context(context);
    let mut parts = vec![query.to_string()];
    let mut room = QUERY_LIMIT_V2 as i64 - query.chars().count() as i64;
    if !last.is_empty() && room > 0 {
        let n = last.chars().count().min(400.max(room * 3 / 5) as usize);
        let take = last_chars(&last, n);
        room -= take.chars().count() as i64;
        parts.push(format!("Last tool output:\n{take}"));
    }
    if !earlier.is_empty() && room > 100 {
        parts.push(format!(
            "Earlier context:\n{}",
            last_chars(&earlier, room as usize)
        ));
    }
    format!("{}{}", granularity.instruction(), parts.join("\n"))
}

/// The task text for a past commit when fitting the personal adapter: the subject plus the
/// first body line when it is short, as evaluated on real repository histories.
pub fn history_body(subject: &str, body: &str) -> String {
    let first = split_lines(body)
        .into_iter()
        .map(py_strip)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut query = py_strip(subject).to_string();
    if !first.is_empty() && first.chars().count() <= 160 {
        query.push('\n');
        query.push_str(first);
    }
    query
}

/// The full query text for a past commit (instruction + [`history_body`]); equal to
/// `query_text(history_body(..), "", File)`.
pub fn history_query(subject: &str, body: &str) -> String {
    format!(
        "{INSTRUCT_FILE}{}",
        take_chars(&history_body(subject, body), QUERY_LIMIT)
    )
}

/// Counts model tokens for a text (the embedding backend's tokenizer).
pub trait TokenCounter {
    /// Number of tokens including special tokens.
    fn count_tokens(&self, text: &str) -> usize;
}

/// What [`fit_query`] had to drop to fit the model's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Truncation {
    /// Tokens in the final query text, when a counter was available.
    pub query_tokens: Option<usize>,
    /// Characters removed from the start (oldest part) of the context.
    pub context_chars_dropped: usize,
}

/// [`query_text`], trimmed so the model's token limit cuts the oldest context rather than
/// silently truncating the newest.
pub fn fit_query(
    counter: Option<&dyn TokenCounter>,
    query: &str,
    context: &str,
    granularity: Granularity,
    max_seq: usize,
) -> (String, Truncation) {
    let mut text = query_text(query, context, granularity);
    let Some(counter) = counter.filter(|_| !context.is_empty()) else {
        return (text, Truncation::default());
    };
    let chars: Vec<(usize, char)> = context.char_indices().collect();
    let len = chars.len();
    let mut n = counter.count_tokens(&text);
    let mut dropped = 0usize;
    while n > max_seq && len - dropped > 200 {
        let remaining = (len - dropped) as f64;
        let cut = 200.max((remaining * (1.0 - max_seq as f64 / n as f64)) as usize + 50);
        dropped = (len - 200).min(dropped + cut);
        let start = chars.get(dropped).map(|(i, _)| *i).unwrap_or(context.len());
        text = query_text(query, &context[start..], granularity);
        n = counter.count_tokens(&text);
    }
    (
        text,
        Truncation {
            query_tokens: Some(n),
            context_chars_dropped: dropped,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CharsAsTokens;
    impl TokenCounter for CharsAsTokens {
        fn count_tokens(&self, text: &str) -> usize {
            text.chars().count() / 4
        }
    }

    #[test]
    fn fit_query_drops_oldest_context_first() {
        let context = format!("OLD{}NEW", "x".repeat(3000));
        let (text, t) = fit_query(
            Some(&CharsAsTokens),
            "fix it",
            &context,
            Granularity::File,
            MAX_SEQ,
        );
        assert!(text.ends_with("NEW"));
        assert!(!text.contains("OLD"));
        assert!(t.context_chars_dropped > 0);
        assert!(t.query_tokens.unwrap() <= MAX_SEQ);
    }

    #[test]
    fn fit_query_without_counter_is_plain_query_text() {
        let (text, t) = fit_query(None, "q", "ctx", Granularity::File, MAX_SEQ);
        assert_eq!(text, query_text("q", "ctx", Granularity::File));
        assert_eq!(t, Truncation::default());
    }
}
