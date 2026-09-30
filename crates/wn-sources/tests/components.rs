use wn_sources::{file_doc, kind_of, Kind};

#[test]
fn components_are_indexed_with_script_and_template_text() {
    for path in ["src/App.vue", "src/App.svelte", "src/App.astro"] {
        let source = "<script>const version = 'current';</script>\n<main>Choose a version</main>";
        assert_eq!(kind_of(path), Some(Kind::Source), "{path}");
        let doc = file_doc(path, source);
        assert!(doc.contains("version"), "{path}: {doc}");
        assert!(doc.contains("Choose a version"), "{path}: {doc}");
    }
}

#[test]
fn vue_template_survives_a_long_script() {
    let source = format!(
        "<script>const version = 'current'; {}</script><template><main>Choose a version</main></template>",
        "let filler = 1; ".repeat(200)
    );
    let doc = file_doc("src/App.vue", &source);
    assert!(doc.contains("version"));
    assert!(doc.contains("Choose a version"));
}

#[test]
fn vue_nested_template_keeps_outer_markup_and_does_not_repeat_it() {
    let source = "<script>const ready = true;</script><template><template v-if=\"ready\"><span>inside</span></template><p>after nested template</p></template>";
    let doc = file_doc("src/App.vue", source);
    assert!(doc.contains("after nested template"), "{doc}");
    assert_eq!(doc.matches("inside").count(), 1, "{doc}");
    assert_eq!(doc.matches("after nested template").count(), 1, "{doc}");
    assert!(doc.contains("const ready = true"), "{doc}");
}
