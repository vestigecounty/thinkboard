//! Binding generator entry point. Run as:
//!   cargo run -p thinkboard-ffi --bin uniffi-bindgen -- \
//!     generate --library <path-to-libthinkboard_ffi> --language swift --out-dir <dir>
fn main() {
    uniffi::uniffi_bindgen_main()
}
