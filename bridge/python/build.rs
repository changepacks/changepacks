fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/endian_helper.c");
    let target = std::env::var("TARGET").unwrap_or_default();

    // Build endian_helper for all Linux targets (needed for manylinux2014 and older)
    if target.contains("linux") {
        let mut build = cc::Build::new();
        build.file("src/endian_helper.c");

        // Enable position-independent code for shared libraries
        build.flag("-fPIC");

        build.compile("endian_helper");

        // For older linkers (manylinux2014, ARM, etc.), we need to be more explicit
        if target != "x86_64-unknown-linux-gnu" {
            println!("cargo:rustc-link-arg-bins=-Wl,--no-as-needed");
            // Force include all symbols from the static library
            println!("cargo:rustc-link-arg-bins=-Wl,--whole-archive");
            println!("cargo:rustc-link-arg-bins=-Wl,-lendian_helper");
            println!("cargo:rustc-link-arg-bins=-Wl,--no-whole-archive");
        }
    }
}
