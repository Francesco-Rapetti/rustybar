//! Compiles the helper of now_playing's `other_apps` (src/widgets/now_playing.m) into a dylib in
//! OUT_DIR, which the binary carries with `include_bytes!` and writes to the cache when it needs it:
//! /usr/bin/perl loads it, as MediaRemote answers only Apple's processes. For both architectures,
//! whichever the binary's: perl runs as the Mac's own. Clang comes with the Command Line Tools,
//! which Rust needs on macOS anyway (to link).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let source = "src/widgets/now_playing.m";
    println!("cargo:rerun-if-changed={source}");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("now_playing.dylib");
    let status = Command::new("xcrun")
        .args(["clang", "-dynamiclib", "-fobjc-arc", "-Os", "-Wall", "-Werror", "-arch", "arm64", "-arch", "x86_64"])
        .args(["-mmacosx-version-min=13.0", "-framework", "Foundation", "-o"])
        .arg(&out)
        .arg(source)
        .status()
        .expect("xcrun clang: the Command Line Tools");
    assert!(status.success(), "the helper of now_playing didn't compile");
}
