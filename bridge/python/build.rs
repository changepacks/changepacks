fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("linux") && target.contains("gnu") {
        cc::Build::new()
            .file("src/endian_helper.c")
            .compile("endian_helper");
    }
}
