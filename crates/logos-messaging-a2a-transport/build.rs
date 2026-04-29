fn main() {
    #[cfg(feature = "logos-core")]
    {
        println!("cargo:rustc-link-lib=logos_core");
        if let Ok(dir) = std::env::var("LOGOS_CORE_LIB_DIR") {
            println!("cargo:rustc-link-search={}", dir);
        }
    }

    #[cfg(feature = "logos-delivery")]
    {
        println!("cargo:rustc-link-lib=dylib=logosdelivery");
        if let Ok(dir) = std::env::var("LIBLOGOSDELIVERY_LIB_DIR") {
            println!("cargo:rustc-link-search=native={}", dir);
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir);
        }
        println!("cargo:rerun-if-env-changed=LIBLOGOSDELIVERY_LIB_DIR");
    }
}
