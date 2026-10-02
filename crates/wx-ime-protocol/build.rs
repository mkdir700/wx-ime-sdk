fn main() {
    println!("cargo:rerun-if-changed=proto/wetype.proto");
    println!("cargo:rerun-if-changed=proto/flurry.proto");
    let descriptors =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"))
            .join("wetype-descriptors.bin");
    prost_build::Config::new()
        .file_descriptor_set_path(descriptors)
        .compile_protos(&["proto/wetype.proto", "proto/flurry.proto"], &["proto"])
        .expect("compile verified WeType schema");
}
