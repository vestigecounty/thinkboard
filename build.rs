//! Build script: embeds the application icon (and version metadata) into the
//! Windows executable, so it shows in Explorer, the taskbar, and Alt+Tab.
//!
//! A no-op when not targeting Windows. When cross-compiling from another host,
//! `winresource` derives the MinGW resource compiler name from the target
//! triple (`x86_64-w64-mingw32-windres`/`-ar`), which the mingw-w64 toolchain
//! provides.

fn main() {
    println!("cargo:rerun-if-changed=assets/thinkboard.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/thinkboard.ico");
        if let Err(error) = res.compile() {
            // Don't fail the build over a missing icon — just warn.
            println!("cargo:warning=could not embed Windows icon: {error}");
        }
    }
}
