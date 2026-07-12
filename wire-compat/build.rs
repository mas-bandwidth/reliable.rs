fn main() {
    println!("cargo:rerun-if-changed=c");
    cc::Build::new()
        .file("c/reliable.c")
        .file("c/shim.c")
        .include("c")
        .define("RELIABLE_DEBUG", None)
        .compile("reliable_c");
}
