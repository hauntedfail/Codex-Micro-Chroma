use std::{env, path::PathBuf};

fn main() {
    const MINIMUM_MACOS_VERSION: &str = "14.2";

    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-changed=src/process_tap.h");
    println!("cargo:rerun-if-changed=src/process_tap.m");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    cc::Build::new()
        .file("src/process_tap.m")
        .flag("-fobjc-arc")
        .flag("-Werror=return-type")
        .flag(format!("-mmacosx-version-min={MINIMUM_MACOS_VERSION}"))
        .compile("codex_micro_chroma_process_tap");

    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-arg=-mmacosx-version-min={MINIMUM_MACOS_VERSION}");

    let plist = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("Info.plist");
    println!(
        "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist.display()
    );
}
