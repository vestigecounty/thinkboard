//! The ThinkBoard binary — a thin entry point over the `thinkboard` library,
//! which holds the app itself (see `lib.rs`).

// On Windows, a release build is a GUI app (no console window), while debug
// builds keep the console so stderr (eprintln diagnostics, panics) stays
// visible. The `no-console` feature forces the GUI subsystem in any profile —
// e.g. `--features no-console` for a clean, console-free debug build to test.
#![cfg_attr(
    all(windows, any(not(debug_assertions), feature = "no-console")),
    windows_subsystem = "windows"
)]

fn main() -> iced::Result {
    // Route iced/wgpu's `log` output to stderr. Defaults to warnings; set e.g.
    // `RUST_LOG=iced_wgpu=info` to see the selected adapter, backend, and
    // surface alpha mode (useful for diagnosing overlay transparency).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    thinkboard::run()
}
