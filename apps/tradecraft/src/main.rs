#![forbid(unsafe_code)]
#![cfg_attr(windows, windows_subsystem = "windows")]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

fn main() {
    if let Err(error) = tradecraft_ui_egui::run() {
        eprintln!("TradeCraft could not start: {error}");
        std::process::exit(1);
    }
}
