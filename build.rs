fn main() {
    // Prost messages are vendored in src/easyrpc/conformance/v1/mod.rs; we do not
    // re-run prost-build (keeps builds offline). Copy output into src/ on gen.
    println!("cargo:rerun-if-changed=buf.yaml");
}
