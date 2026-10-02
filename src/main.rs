mod model;
mod paths;
mod target;

fn main() {}

#[cfg(test)]
mod testutil {
    use std::path::PathBuf;

    /// A fresh directory under `target/` (never /tmp, a small tmpfs).
    pub fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-scratch").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
