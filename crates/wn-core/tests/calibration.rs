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
