// Two-mode build:
//
// - **`LOGOS_CPP_SDK_DIR` set** → real build. cmake + bindgen against
//   the shim, link the static archives. Emits `cargo:rustc-cfg=logos_core_real`
//   so lib.rs picks the working impl.
// - **`LOGOS_CPP_SDK_DIR` unset** → stub build. No cmake, no bindgen.
//   lib.rs falls back to a stub `Shim` whose ctor returns an error.
//   `cargo build --workspace` passes on a Qt-less machine.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=LOGOS_CPP_SDK_DIR");
    println!("cargo:rerun-if-changed=shim/shim.h");
    println!("cargo:rerun-if-changed=shim/shim.cpp");
    println!("cargo:rerun-if-changed=shim/CMakeLists.txt");
    println!("cargo:rerun-if-changed=wrapper.h");

    let Ok(sdk_dir) = env::var("LOGOS_CPP_SDK_DIR") else {
        write_stub_bindings();
        return;
    };

    // ── Real build ──────────────────────────────────────────────────
    let dst = cmake::Config::new("shim")
        .define("LOGOS_CPP_SDK_DIR", &sdk_dir)
        .define("CMAKE_BUILD_TYPE", "Release")
        .build();

    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=logos_shim");
    println!("cargo:rustc-link-lib=static=logos_sdk");

    for lib in &["Qt6Core", "Qt6Network", "Qt6RemoteObjects"] {
        println!("cargo:rustc-link-lib={lib}");
    }
    // New pre-built logos-cpp-sdk (≥ May 2026) bundles plain-transport with
    // OpenSSL/Boost.Asio SSL support compiled in. Link those explicitly since
    // we bypass find_package(logos-cpp-sdk) to avoid Boost/OpenSSL discovery.
    println!("cargo:rustc-link-lib=ssl");
    println!("cargo:rustc-link-lib=crypto");
    println!("cargo:rustc-link-lib=stdc++");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}/include", dst.display()))
        .clang_arg("-Ishim")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_function("logos_shim_.*")
        .allowlist_type("LogosShim")
        .generate()
        .expect("bindgen failed");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    bindings.write_to_file(&out).expect("write bindings.rs");

    println!("cargo:rustc-cfg=logos_core_real");
    // Tell rustc about the custom cfg so we don't get an unexpected-cfg
    // warning on consumer crates.
    println!("cargo:rustc-check-cfg=cfg(logos_core_real)");
}

fn write_stub_bindings() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    std::fs::write(
        &out,
        "// stub: LOGOS_CPP_SDK_DIR not set at build time. \
         Set it to a logos-cpp-sdk checkout to enable real bindings.\n",
    )
    .expect("write stub bindings.rs");
    println!("cargo:rustc-check-cfg=cfg(logos_core_real)");
}
