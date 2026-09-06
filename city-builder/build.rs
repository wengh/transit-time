//! Fingerprint the preprocessing source so `metadata.json` can tell whether
//! a `.bin` was built by the current code. Comparing mtimes did not work in
//! CI: `actions/checkout` writes every source file at checkout time while
//! the restored `.bin` files keep their original (older) mtimes, so every
//! city looked out of date on every scheduled run.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    use sha1::{Digest, Sha1};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("city-builder lives one level below the workspace root")
        .to_path_buf();
    let dirs = [
        root.join("transit-data/src"),
        root.join("transit-prep/src"),
        root.join("city-builder/src"),
    ];

    let mut files = Vec::new();
    for dir in &dirs {
        collect(dir, &mut files);
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    files.sort();

    let mut hasher = Sha1::new();
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap_or(file);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(std::fs::read(file).expect("read source file"));
        hasher.update(b"\0");
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    println!("cargo:rustc-env=CODE_FINGERPRINT={hex}");
}
