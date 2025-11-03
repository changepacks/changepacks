fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/endian_helper.c");
    let target = std::env::var("TARGET").unwrap_or_default();

    // Build endian_helper for all Linux targets (including ARM with gnueabihf)
    if target.contains("linux") {
        let mut build = cc::Build::new();
        build.file("src/endian_helper.c");

        // Set target if cross-compiling
        if let Ok(target) = std::env::var("TARGET") {
            build.target(&target);
        }

        build.compile("endian_helper");

        // Ensure the static library is not dropped due to --as-needed and link order.
        println!("cargo:rustc-link-lib=static=endian_helper");
        println!("cargo:rustc-link-arg-bins=-Wl,--whole-archive");
        println!("cargo:rustc-link-arg-bins=-Wl,--no-whole-archive");
    }
}
