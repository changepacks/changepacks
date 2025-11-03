fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("linux") && target.contains("gnu") {
        cc::Build::new()
            .file("src/endian_helper.c")
            .compile("endian_helper");

        // Ensure the static library is not dropped due to --as-needed and link order.
        // Place it effectively at the end using link args and keep all objects.
        println!("cargo:rustc-link-lib=static=endian_helper");
        println!("cargo:rustc-link-arg-bins=-Wl,--whole-archive");
        println!("cargo:rustc-link-arg-bins=-Wl,--no-whole-archive");

        // Don't link external libbsd; provide symbols ourselves in endian_helper.c
    }
}
