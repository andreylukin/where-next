//! `wn`: a fast local "where next" hint for coding agents and developers.

use clap::Parser;

fn main() {
    let (text, code) = wn_cli::run(wn_cli::Cli::parse());
    if text.is_empty() {
        // `wn mcp` already used stdout for the protocol.
    } else if code == 0 || code == wn_cli::update::EXIT_UPDATE_AVAILABLE {
        println!("{text}");
    } else {
        eprintln!("{text}");
    }
    std::process::exit(code);
}
