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
