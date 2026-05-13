fn main() {
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
