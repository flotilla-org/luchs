use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-env-changed=CEF_ROOT");
    println!("cargo:rerun-if-env-changed=CEF_WRAPPER_LIB");
    if env::var_os("CARGO_FEATURE_CEF").is_none() {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_OS").unwrap(),
        "macos",
        "CEF spike currently requires macOS"
    );
    let root = PathBuf::from(
        env::var_os("CEF_ROOT")
            .expect("Run scripts/build-cef.py to build the optional CEF renderer"),
    );
    let wrapper = PathBuf::from(env::var_os("CEF_WRAPPER_LIB").expect("CEF_WRAPPER_LIB"));
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../native/cef/engine.mm");
    println!("cargo:rerun-if-changed={}", source.display());
    println!(
        "cargo:rerun-if-changed={}",
        source.with_file_name("bridge.h").display()
    );
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out.join("engine.o");
    assert!(
        Command::new("clang++")
            .args([
                "-std=c++20",
                "-fobjc-arc",
                "-O2",
                "-g",
                "-DCEF_USE_SANDBOX=1",
                "-I"
            ])
            .arg(&root)
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .status()
            .expect("failed to start clang++ for the CEF shim")
            .success(),
        "CEF shim compilation failed"
    );
    assert!(
        Command::new("ar")
            .arg("rcs")
            .arg(out.join("libluchs_cef.a"))
            .arg(object)
            .status()
            .expect("failed to start ar for the CEF shim archive")
            .success()
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=luchs_cef");
    println!("cargo:rustc-link-search=native={}", wrapper.display());
    println!("cargo:rustc-link-lib=static=cef_dll_wrapper");
    println!("cargo:rustc-link-lib=c++");
    for framework in [
        "AppKit",
        "WebKit",
        "Metal",
        "QuartzCore",
        "IOSurface",
        "Security",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
