//! Snuffler -- GhostBuster for macOS. Two repairs for Excel workbooks that have
//! grown unusable.
//!
//!   BUST    removes the invisible, zero-size pictures Excel accumulates in a
//!           worksheet when images are pasted onto it over and over.
//!   SQUISH  re-encodes the workbook's images, which Excel often stores as raw
//!           uncompressed bitmaps inside metafile wrappers.
//!
//! The window is AppKit and exists only on macOS. Everything else -- the
//! repairs, the window's state machine, headless mode -- is platform-neutral,
//! so it builds and tests anywhere Rust does.
//!
//! Headless, for the conformance suite:
//!   Snuffler --headless [--bust] [--squish High|Med|XSmall] --json out.json <file>...

// Off macOS only headless mode is compiled in, which leaves the window's
// state machine unused outside of tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

mod app;
mod clean;
mod headless;
mod squish;

#[cfg(target_os = "macos")]
mod gui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--headless") {
        std::process::exit(headless::run(&args));
    }

    #[cfg(target_os = "macos")]
    gui::run(args);

    #[cfg(not(target_os = "macos"))]
    {
        eprintln!(
            "Snuffler's window is macOS-only. Here it runs headless:\n  \
             snuffler --headless [--bust] [--squish High|Med|XSmall] [--json out.json] <file>..."
        );
        std::process::exit(2);
    }
}
