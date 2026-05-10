// Drives the cmake build of the C++ shim and runs bindgen over its
// narrow C ABI header. End result: `cargo build` produces a
// statically-linked binary that can call LogosAPI from Rust without
// ever exposing a Qt type to the Rust crate.

use std::env;
use std::path::PathBuf;

fn main() {
    let sdk_dir = env::var("LOGOS_CPP_SDK_DIR")
        .expect("LOGOS_CPP_SDK_DIR must be set — points at a logos-cpp-sdk checkout.");

    // 1. Build the shim + transitively the SDK static lib via cmake.
    let dst = cmake::Config::new("shim")
        .define("LOGOS_CPP_SDK_DIR", &sdk_dir)
        .define("CMAKE_BUILD_TYPE", "Release")
        .build();

    // 2. Tell rustc where the static archives live + link them.
    //    Both `liblogos_shim.a` and `liblogos_sdk.a` get installed to
    //    `<install_prefix>/lib/` by the shim's CMakeLists install rules.
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=logos_shim");
    println!("cargo:rustc-link-lib=static=logos_sdk");

    // 3. Qt + transitive C++ deps. Qt6 puts its libs on the linker
    //    search path via wrapQtAppsHook in nix; we just have to name
    //    them. The order matters for static-link resolution.
    for lib in &["Qt6Core", "Qt6Network", "Qt6RemoteObjects"] {
        println!("cargo:rustc-link-lib={lib}");
    }
    // SDK pulls Boost.System + OpenSSL + nlohmann_json (header-only,
    // no link). Plus stdc++ for the C++ runtime.
    println!("cargo:rustc-link-lib=boost_system");
    println!("cargo:rustc-link-lib=ssl");
    println!("cargo:rustc-link-lib=crypto");
    println!("cargo:rustc-link-lib=stdc++");

    // 4. bindgen over the C ABI header. The shim's interface is C-only
    //    (extern "C" guarded), so we don't need clang's C++ flags.
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}/include", dst.display()))
        // The shim's own dir, in case cmake didn't install the header.
        .clang_arg("-Ishim")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        // Be conservative — only emit bindings for the shim's symbols,
        // not whatever stdlib types they happen to mention.
        .allowlist_function("logos_shim_.*")
        .allowlist_type("LogosShim")
        .generate()
        .expect("bindgen failed");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    bindings.write_to_file(&out).expect("write bindings.rs");

    println!("cargo:rerun-if-changed=shim/shim.h");
    println!("cargo:rerun-if-changed=shim/shim.cpp");
    println!("cargo:rerun-if-changed=shim/CMakeLists.txt");
    println!("cargo:rerun-if-changed=wrapper.h");
}
