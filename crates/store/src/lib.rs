//! On-disk layout for the launcher (versions, libraries, assets, instances).

use std::path::PathBuf;

#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        Paths {
            root: PathBuf::from(home).join("Library/Application Support/zenith-launcher"),
        }
    }
    pub fn libraries(&self) -> PathBuf {
        self.root.join("libraries")
    }
    pub fn assets(&self) -> PathBuf {
        self.root.join("assets")
    }
    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.root.join("versions").join(id)
    }
    pub fn natives_dir(&self, id: &str) -> PathBuf {
        self.version_dir(id).join("natives")
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self::new()
    }
}
