use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let manifest_directory =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let plist_template = manifest_directory.join("packaging/macos/Info.plist");

    println!("cargo:rerun-if-changed={}", plist_template.display());
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let version = env::var("CARGO_PKG_VERSION").expect("package version");
        let version_core = version
            .split_once('-')
            .map_or(version.as_str(), |(core, _)| core);
        let plist = env::var_os("OUT_DIR")
            .map(PathBuf::from)
            .expect("build output directory")
            .join("Info.plist");
        let template = fs::read_to_string(&plist_template).expect("read Info.plist template");
        let plist_contents = template
            .replace("{{SOSUS_VERSION_CORE}}", version_core)
            .replace("{{SOSUS_VERSION}}", &version);
        fs::write(&plist, plist_contents).expect("write generated Info.plist");
        println!(
            "cargo:rustc-link-arg-bin=sosus=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
        link_macos_compiler_runtime();
    }
}

/// Newer Clang emits `___isPlatformVersionAtLeast` for Metal availability
/// checks. Rust's final link does not add the Clang runtime automatically, so
/// explicitly link it for whisper.cpp's Metal objects.
fn link_macos_compiler_runtime() {
    let clang = Command::new("xcrun")
        .args(["--find", "clang"])
        .output()
        .expect("locate clang with xcrun");
    assert!(clang.status.success(), "xcrun could not locate clang");
    let clang = PathBuf::from(
        String::from_utf8(clang.stdout)
            .expect("clang path is UTF-8")
            .trim(),
    );
    let toolchain = clang
        .parent()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .expect("clang path has a toolchain root");
    let clang_root = toolchain.join("usr/lib/clang");
    let version = std::fs::read_dir(&clang_root)
        .expect("read Clang runtime directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("lib/darwin/libclang_rt.osx.a").is_file())
        .expect("locate macOS Clang runtime");

    println!(
        "cargo:rustc-link-search=native={}",
        version.join("lib/darwin").display()
    );
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
