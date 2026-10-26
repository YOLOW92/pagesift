use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=ESSENCE_SOURCE");
    let source = env::var_os("ESSENCE_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../../../data/six/sources/essence/backend/src/format"));
    let markdown = source.join("markdown.rs").canonicalize().unwrap();
    let image = source.join("image_processing.rs").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", markdown.display());
    println!("cargo:rerun-if-changed={}", image.display());
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("essence.rs"),
        format!("pub mod format {{ #[path = {markdown:?}] pub mod markdown; #[path = {image:?}] pub mod image_processing; }}")).unwrap();
}
