use std::{env, path::PathBuf};

fn main() {
    let plist = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("packaging/macos/Info.plist");

    println!("cargo:rerun-if-changed={}", plist.display());
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!(
            "cargo:rustc-link-arg-bin=sosus=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
