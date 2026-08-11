use std::{env, path::PathBuf, process::Command};

fn main() {
    const MINIMUM_MACOS_VERSION: &str = "14.2";

    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-changed=src/process_tap.h");
    println!("cargo:rerun-if-changed=src/process_tap.m");
    println!("cargo:rerun-if-changed=src/media_sessions.m");
    println!("cargo:rerun-if-changed=src/media_sessions.pl");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    cc::Build::new()
        .file("src/process_tap.m")
        .flag("-fobjc-arc")
        .flag("-Werror=return-type")
        .flag(format!("-mmacosx-version-min={MINIMUM_MACOS_VERSION}"))
        .compile("codex_micro_chroma_process_tap");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    let helper = out_dir.join("libcodex_micro_chroma_media_sessions.dylib");
    let target_arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        Ok(other) => panic!("unsupported macOS target architecture: {other}"),
        Err(error) => panic!("missing target architecture: {error}"),
    };
    let clang_status = Command::new("xcrun")
        .args([
            "clang",
            "-dynamiclib",
            "-fobjc-arc",
            "-fblocks",
            "-Wall",
            "-Wextra",
            "-Werror=return-type",
            "-arch",
            target_arch,
            &format!("-mmacosx-version-min={MINIMUM_MACOS_VERSION}"),
            "-framework",
            "Foundation",
            "-o",
        ])
        .arg(&helper)
        .arg("src/media_sessions.m")
        .status()
        .expect("failed to launch clang for the MediaRemote session helper");
    assert!(
        clang_status.success(),
        "failed to compile MediaRemote session helper"
    );

    let codesign_status = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&helper)
        .status()
        .expect("failed to launch codesign for the MediaRemote session helper");
    assert!(
        codesign_status.success(),
        "failed to ad-hoc sign MediaRemote session helper"
    );

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
