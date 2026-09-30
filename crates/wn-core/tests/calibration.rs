//! Abstain calibration: thresholds come from a per-model calibration file, keyed by query kind
//! (a short request, an issue-style start, an error, a conversational follow-up) and by whether
//! the personal adapter applied. A kind without thresholds never abstains.

use wn_core::rank::{abstain_reason, abstain_with, Calibration, Hint, QueryKind};

fn hints(top: f64, second: f64) -> Vec<Hint> {
    [("a.py", top), ("b.py", second)]
        .into_iter()
        .map(|(p, s)| Hint {
            path: p.into(),
            similarity: s,
            evidence: None,
            name: None,
            line: None,
        })
        .collect()
}

#[test]
fn query_kinds_are_classified_from_text() {
    assert_eq!(
        QueryKind::classify("fix login session expiry", ""),
        QueryKind::Request
    );
    let issue = "Unable to pickle figure with draggable legend\n\n### Bug summary\nI am unable to pickle...";
    assert_eq!(QueryKind::classify(issue, ""), QueryKind::Issue);
    assert_eq!(
        QueryKind::classify(&"word ".repeat(60), ""),
        QueryKind::Issue
    );
    assert_eq!(
        QueryKind::classify(
            "why does this fail",
            "Last tool output:\nTraceback (most recent call last):\n  KeyError: 'id'"
        ),
        QueryKind::Error
    );
    assert_eq!(
        QueryKind::classify("tests are red", "test_upload FAILED"),
        QueryKind::Error
    );
    assert_eq!(
        QueryKind::classify("panic in the scheduler", ""),
        QueryKind::Error
    );
    assert_eq!(
        QueryKind::classify(
            "now do the same for the other handler",
            "user: fix the login handler\nassistant: done"
        ),
        QueryKind::Conversational
    );
}

#[test]
fn builtin_v2b_calibration_keeps_the_reference_thresholds() {
    let c = Calibration::v2b();
    for (adapted, strict) in [(false, false), (true, false), (false, true), (true, true)] {
        for (top, second) in [
            (0.9, 0.2),
            (0.3, 0.29),
            (0.5, 0.1),
            (0.2, 0.1),
            (0.58, 0.57),
        ] {
            let h = hints(top, second);
            let th = c.thresholds(QueryKind::Request, adapted, strict).unwrap();
            assert_eq!(
                abstain_with(&h, th),
                abstain_reason(&h, adapted, strict, false),
                "top {top} second {second} adapted {adapted} strict {strict}"
            );
        }
    }
}

#[test]
fn issue_start_hints_are_not_withheld_until_calibrated() {
    // The agent trial found plain v2b withheld 24/50 task-start hints on issue text, 20 of
    // which were right: issue queries have no thresholds, so they always answer.
    let c = Calibration::v2b();
    assert_eq!(c.thresholds(QueryKind::Issue, false, false), None);
    assert_eq!(c.thresholds(QueryKind::Issue, true, true), None);
}

#[test]
fn unlisted_kinds_use_the_default_thresholds() {
    let json = r#"{"model": "m", "kinds": {"default": {
        "adapter": {"min_top": 0.1, "min_margin": 0.0},
        "plain": {"min_top": 0.5, "min_margin": 0.01},
        "strict_adapter": {"min_top": 0.5, "min_margin": 0.07},
        "strict_plain": {"min_top": 0.6, "min_margin": 0.06}},
        "issue": null}}"#;
    let c: Calibration = serde_json::from_str(json).unwrap();
    let plain = c.thresholds(QueryKind::Error, false, false).unwrap();
    assert_eq!((plain.min_top, plain.min_margin), (0.5, 0.01));
    assert_eq!(c.thresholds(QueryKind::Issue, false, false), None);
    assert!(abstain_with(&hints(0.4, 0.1), plain).is_some());
    assert!(abstain_with(&hints(0.6, 0.1), plain).is_none());
}

#[test]
fn a_calibration_without_default_never_abstains_for_unlisted_kinds() {
    let c: Calibration = serde_json::from_str(r#"{"model": "m", "kinds": {}}"#).unwrap();
    assert_eq!(c.thresholds(QueryKind::Request, false, false), None);
}

#[test]
fn error_kind_needs_a_real_error_not_the_word() {
    use wn_core::rank::has_error;
    for text in [
        "TypeError: cannot read properties of undefined\n    at render (app.js:10:5)",
        "src/main.go:12:3: error: undefined: foo",
        "thread 'main' panicked at src/lib.rs:4:5",
        "FAILED tests/test_api.py::test_login - AssertionError",
        "npm ERR! code ELIFECYCLE",
        "error[E0382]: borrow of moved value: `x`",
    ] {
        assert!(has_error(text), "{text}");
    }
    for text in [
        "Rename ErrorBoundary to FallbackBoundary",
        "Add an error message to the login form",
        "Handle errors in the parser",
        "Make the retry exception configurable",
    ] {
        assert!(!has_error(text), "{text}");
        assert_eq!(QueryKind::classify(text, ""), QueryKind::Request, "{text}");
    }
    assert_eq!(
        QueryKind::classify(
            "the build fails with an error:\n`cargo test` exits early",
            ""
        ),
        QueryKind::Error
    );
}

#[test]
fn conversational_needs_context_and_a_short_request() {
    let ctx = "Assistant: I updated the settings page.\nEarlier change: add dark mode";
    assert_eq!(
        QueryKind::classify("now do the same for the profile page", ctx),
        QueryKind::Conversational
    );
    assert_eq!(QueryKind::classify(&"x".repeat(400), ctx), QueryKind::Issue);
    assert_eq!(
        QueryKind::classify("now do the same for the profile page", ""),
        QueryKind::Request
    );
}

/// The Rust classifier agrees with the Python reference (`harness_router.querykind.classify`,
/// which labelled the calibration eval) on public benchmark queries.
#[test]
fn classifier_matches_the_python_reference() {
    #[derive(serde::Deserialize)]
    struct Case {
        query: String,
        context: String,
        kind: QueryKind,
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/querykind_cases.json"
    );
    let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let wrong: Vec<String> = cases
        .iter()
        .filter(|c| QueryKind::classify(&c.query, &c.context) != c.kind)
        .map(|c| format!("{:?}: {:.80}", c.kind, c.query.replace('\n', " ")))
        .collect();
    assert!(wrong.is_empty(), "{} mismatches: {wrong:#?}", wrong.len());
}

#[test]
fn v2b_has_fitted_error_thresholds_and_keeps_issue_starts_open() {
    let c = Calibration::v2b();
    assert!(c.thresholds(QueryKind::Error, false, false).is_some());
    assert!(c.thresholds(QueryKind::Error, true, true).is_some());
    assert_eq!(c.thresholds(QueryKind::Issue, false, false), None);
    // Requests and conversational follow-ups keep the reference default.
    assert_eq!(
        c.thresholds(QueryKind::Conversational, false, false),
        c.thresholds(QueryKind::Request, false, false)
    );
}

/// Single-line error queries count as errors without full stack frames: a `Traceback` header, an
/// exception name followed by its message, `panicked at`, or `Error: …`. Class names such as
/// `ErrorBoundary`, or an exception named in passing, do not.
#[test]
fn single_line_error_queries_are_errors() {
    for q in [
        "Traceback: KeyError 'user_id' in handler",
        "TypeError: cannot read properties of undefined (reading 'map')",
        "thread main panicked at index out of bounds",
        "fix the parser, it raises ValueError",
        "got Error: connection refused on startup",
        "Exception in thread \"main\" java.lang.NullPointerException",
    ] {
        assert_eq!(QueryKind::classify(q, ""), QueryKind::Error, "{q}");
    }
    for q in [
        "add an ErrorBoundary around the picker",
        "handle ValueError in parser",
        "rename the error field in the settings form",
    ] {
        assert_eq!(QueryKind::classify(q, ""), QueryKind::Request, "{q}");
    }
}

/// Shipped gemma calibrations leave adapter-mode thresholds at 0.0/0.0 ("never abstain" on the
/// fitting repository). That let gibberish through with 3 hints, so an unset (all-zero) adapter
/// entry means the built-in adapter floor; an explicit negative threshold still never abstains.
#[test]
fn unset_adapter_thresholds_fall_back_to_the_builtin_floor() {
    use wn_core::rank::ADAPTER_FLOOR;
    let json = r#"{"model": "m", "kinds": {"default": {
        "adapter": {"min_top": 0.0, "min_margin": 0.0},
        "plain": {"min_top": 0.3667, "min_margin": 0.0},
        "strict_adapter": {"min_top": 0.5361, "min_margin": 0.0288},
        "strict_plain": {"min_top": 0.6857, "min_margin": 0.0779}},
        "issue": {
        "adapter": {"min_top": -1.0, "min_margin": 0.0},
        "plain": {"min_top": -1.0, "min_margin": 0.0},
        "strict_adapter": {"min_top": 0.5, "min_margin": 0.0},
        "strict_plain": {"min_top": 0.5, "min_margin": 0.0}}}}"#;
    let c: Calibration = serde_json::from_str(json).unwrap();
    let th = c.thresholds(QueryKind::Request, true, false).unwrap();
    assert_eq!(th, ADAPTER_FLOOR);
    // Gibberish-level similarity abstains; a real question just above the floor answers.
    assert!(abstain_with(&hints(0.12, 0.11), th).is_some());
    assert!(abstain_with(&hints(ADAPTER_FLOOR.min_top + 0.01, 0.0), th).is_none());
    // Fitted thresholds are untouched.
    let plain = c.thresholds(QueryKind::Request, false, false).unwrap();
    assert_eq!((plain.min_top, plain.min_margin), (0.3667, 0.0));
    let strict = c.thresholds(QueryKind::Request, true, true).unwrap();
    assert_eq!((strict.min_top, strict.min_margin), (0.5361, 0.0288));
    // An explicit "never abstain" (negative) stays that way.
    let issue = c.thresholds(QueryKind::Issue, true, false).unwrap();
    assert_eq!(issue.min_top, -1.0);
    assert!(abstain_with(&hints(0.01, 0.0), issue).is_none());
}
