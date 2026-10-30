//! The last state each widget drew, kept on disk so that the next start shows it at once (dimmed)
//! while the widgets get their data again.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::runtime;
use crate::ui::Node;

#[derive(Default)]
pub struct Cache {
    nodes: HashMap<String, Node>,
    dirty: bool,
    save_queued: bool,
}

fn path() -> PathBuf {
    crate::config::home().join("Library/Caches/rustybar/bar.json")
}

impl Cache {
    pub fn load() -> Cache {
        let nodes = std::fs::read(path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Cache { nodes, dirty: false, save_queued: false }
    }

    pub fn get(&self, widget: &str, variant: &str) -> Option<&Node> {
        self.nodes.get(&format!("{widget}|{variant}"))
    }

    pub fn put(&mut self, widget: &str, variant: &str, node: &Node) {
        let key = format!("{widget}|{variant}");
        if self.nodes.get(&key) != Some(node) {
            self.nodes.insert(key, node.clone());
            self.dirty = true;
        }
    }

    /// Saves within a few seconds, once for a burst of changes.
    pub fn save_soon(&mut self) {
        if self.dirty && !self.save_queued {
            self.save_queued = true;
            runtime::after(5.0, |app| {
                app.cache.save_queued = false;
                app.cache.save();
            });
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec(&self.nodes) {
            // Write and rename, so a crash never leaves half a file
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &p).is_ok() {
                self.dirty = false;
            }
        }
    }
}
