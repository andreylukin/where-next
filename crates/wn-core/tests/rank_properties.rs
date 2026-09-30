//! Property tests for ranking, budgeting and fusion.

use proptest::prelude::*;
use wn_core::rank::{
    abstain_reason, budget, budget_ask, path_prior, rank_of, rrf, top_k, Hint, Hints, MAX_HINTS,
    TOKEN_BUDGET,
};

#[test]
fn production_paths_win_unless_requested() {
    assert!(path_prior("tests/test_routes.py", "where are routes matched") < 0.0);
    assert_eq!(path_prior("src/routes.py", "where are routes matched"), 0.0);
    assert_eq!(path_prior("tests/test_routes.py", "fix routing tests"), 0.0);
    assert_eq!(path_prior("tests/test_routes.py", ""), 0.0);
    assert!(path_prior("tests/test_routes.py", "latest routes") < 0.0);
    assert!(path_prior("src/test_routes.py", "routes") < 0.0);
    assert!(path_prior("src/routes_test.go", "routes") < 0.0);
    assert!(path_prior("src/routes_spec.ts", "routes") < 0.0);
    assert!(path_prior("src/__mocks__/routes.ts", "routes") < 0.0);
    assert_eq!(path_prior("src/routes_spec.ts", "fix spec failure"), 0.0);
}

#[test]
fn ask_budget_respects_k_and_reserves_requested_kinds() {
    let h = Hints {
        files: vec![hint(8, 0.9, None); 3],
        functions: vec![hint(8, 0.8, Some(8))],
        configs: vec![hint(8, 0.7, None)],
    };
    let one = budget_ask(h.clone(), 1, true, true);
    assert_eq!(
        (one.files.len(), one.functions.len(), one.configs.len()),
        (1, 0, 0)
    );
    let three = budget_ask(h, 3, true, true);
    assert_eq!(
        (
            three.files.len(),
            three.functions.len(),
            three.configs.len()
        ),
        (1, 1, 1)
    );
}

#[test]
fn ask_budget_keeps_last_file_before_optional_slots() {
    let h = Hints {
        files: vec![hint(900, 0.9, None)],
        functions: vec![hint(900, 0.8, Some(20))],
        configs: vec![hint(900, 0.7, None)],
    };
    let out = budget_ask(h, 3, true, true);
    assert_eq!(out.files.len(), 1);
    assert!(out.functions.is_empty());
    assert!(out.configs.is_empty());
}

fn hint(path_len: usize, sim: f64, name: Option<usize>) -> Hint {
    Hint {
        path: "p".repeat(path_len.max(1)),
        similarity: sim,
        evidence: None,
        name: name.map(|n| "n".repeat(n)),
        line: name.map(|_| 1),
    }
}

fn hints_strategy() -> impl Strategy<Value = Hints> {
    let h = |named: bool| {
        (1usize..500, 0.0f64..1.0, 1usize..80)
            .prop_map(move |(l, s, n)| hint(l, s, named.then_some(n)))
    };
    (
        prop::collection::vec(h(false), 0..6),
        prop::collection::vec(h(true), 0..5),
        prop::collection::vec(h(false), 0..4),
    )
        .prop_map(|(files, functions, configs)| Hints {
            files,
            functions,
            configs,
        })
}

proptest! {
    #[test]
    fn top_k_agrees_with_full_sort(
        rows in prop::collection::vec(prop::collection::vec(-1.0f32..1.0, 4), 0..60),
        q in prop::collection::vec(-1.0f32..1.0, 4),
        k in 0usize..10,
    ) {
        let flat: Vec<f32> = rows.iter().flatten().copied().collect();
        let got = top_k(&flat, 4, &q, k);
        let mut all: Vec<(usize, f32)> = rows.iter().enumerate()
            .map(|(i, r)| (i, r.iter().zip(&q).map(|(a, b)| a * b).sum())).collect();
        all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
        all.truncate(k);
        prop_assert_eq!(got, all);
    }

    #[test]
    fn budget_never_exceeds_limits_and_keeps_order(h in hints_strategy()) {
        let out = budget(h.clone());
        let total = out.files.len() + out.functions.len() + out.configs.len();
        prop_assert!(total <= MAX_HINTS + 1, "at most 3 files/functions plus one config");
        prop_assert!(out.files.len() + out.functions.len() <= MAX_HINTS);
        prop_assert!(out.configs.len() <= 1);
        // Kept hints are prefixes of the input lists.
        prop_assert_eq!(&h.files[..out.files.len()], &out.files[..]);
        prop_assert_eq!(&h.functions[..out.functions.len()], &out.functions[..]);
        prop_assert_eq!(&h.configs[..out.configs.len()], &out.configs[..]);
        let cost: usize = out.files.iter().chain(&out.functions).chain(&out.configs)
            .map(|x| x.path.len() + x.name.as_ref().map_or(0, |n| n.len()) + 12).sum::<usize>() / 4;
        prop_assert!(out.files.is_empty() || cost <= TOKEN_BUDGET);
    }

    #[test]
    fn higher_similarity_never_creates_an_abstain(a in 0.0f64..1.0, b in 0.0f64..1.0, bump in 0.0f64..0.5,
                                                   adapted: bool, strict: bool) {
        let (top, second) = if a >= b { (a, b) } else { (b, a) };
        let low = vec![hint(3, top, None), hint(3, second, None)];
        let high = vec![hint(3, top + bump, None), hint(3, second, None)];
        if abstain_reason(&low, adapted, strict, false).is_none() {
            prop_assert!(abstain_reason(&high, adapted, strict, false).is_none());
        }
        prop_assert!(abstain_reason(&low, adapted, strict, true).is_none(), "fallback models never abstain");
    }

    #[test]
    fn rrf_is_a_permutation(perm in Just((0..8).collect::<Vec<usize>>()).prop_shuffle(),
                            perm2 in Just((0..8).collect::<Vec<usize>>()).prop_shuffle()) {
        let mut fused = rrf(&[&perm, &perm2], 60);
        prop_assert_eq!(fused.len(), 8);
        fused.sort_unstable();
        prop_assert_eq!(fused, (0..8).collect::<Vec<_>>());
        prop_assert_eq!(rrf(&[&perm], 60), perm.clone());
        prop_assert_eq!(rank_of(&perm, &[perm[3]]), Some(3));
    }
}
