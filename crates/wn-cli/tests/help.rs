//! The `--help` text (with examples) is part of the interface: snapshot it so changes are reviewed.

use clap::CommandFactory;
use wn_cli::Cli;

fn help(path: &[&str]) -> String {
    let mut cmd = Cli::command().term_width(100);
    cmd.build();
    let mut cur = &mut cmd;
    for name in path {
        cur = cur
            .find_subcommand_mut(name)
            .unwrap_or_else(|| panic!("no subcommand {name}"));
    }
    cur.render_long_help().to_string()
}

#[test]
fn help_texts() {
    for (name, path) in [
        ("wn", &[][..]),
        ("init", &["init"][..]),
        ("ask", &["ask"][..]),
        ("bench", &["bench"][..]),
        ("report", &["report"][..]),
        ("stats", &["stats"][..]),
        ("skill_sync", &["skill", "sync"][..]),
    ] {
        insta::assert_snapshot!(format!("help_{name}"), help(path));
    }
}

#[test]
fn examples_mention_commands_that_exist() {
    let text = help(&[]) + &help(&["ask"]) + &help(&["skill", "sync"]);
    for word in [
        "wn init",
        "wn status",
        "wn ask",
        "wn bench",
        "wn stats",
        "wn skill sync",
        "--context-file",
        "--start",
        "--dry-run",
    ] {
        assert!(text.contains(word), "help should show {word}");
    }
}
