//! Virtual DataModel built from Studio Script Sync output.
//!
//! Preferred source is `sourcemap.json` (name/className/filePaths/children). Without
//! it, the tree is derived from the folder layout using Script Sync's suffixes:
//! `.luau` ModuleScript, `.server.luau` Script, `.local.luau` LocalScript,
//! `.client.luau` Script (RunContext Client), `init*.luau` = the folder's own script.
//!
//! Studio doesn't always refresh `sourcemap.json`, so the loaded tree is the sourcemap
//! merged with the folder layout, and `write_sourcemap` can write that merged view back
//! in Studio's format (dropping entries whose file no longer exists).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub type NodeId = usize;

pub const ROOT: NodeId = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunContext {
    Legacy,
    Server,
    Client,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub class_name: String,
    /// Script file, relative to the tree root, with forward slashes.
    pub file: Option<String>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

impl Node {
    pub fn is_module(&self) -> bool {
        self.class_name == "ModuleScript"
    }

    pub fn is_runnable(&self) -> bool {
        self.class_name == "Script" || self.class_name == "LocalScript"
    }

    /// Script Sync encodes RunContext in the suffix; the sourcemap doesn't carry it.
    pub fn run_context(&self) -> RunContext {
        match self.file.as_deref() {
            Some(f) if f.ends_with(".client.luau") || f.ends_with(".client.lua") => {
                RunContext::Client
            }
            Some(f) if f.ends_with(".server.luau") || f.ends_with(".server.lua") => {
                RunContext::Server
            }
            _ => RunContext::Legacy,
        }
    }
}

#[derive(Debug)]
pub struct Tree {
    pub root_dir: PathBuf,
    pub nodes: Vec<Node>,
    by_file: HashMap<String, NodeId>,
    /// Scripts found on disk that sourcemap.json didn't list.
    pub stale_files: Vec<String>,
    /// Scripts sourcemap.json lists that no longer exist on disk.
    pub missing_files: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourcemapNode {
    name: String,
    class_name: String,
    #[serde(default)]
    file_paths: Vec<String>,
    #[serde(default)]
    children: Vec<SourcemapNode>,
}

/// One node in Studio's key order.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourcemapOut<'a> {
    name: &'a str,
    class_name: &'a str,
    file_paths: Vec<&'a str>,
    children: Vec<SourcemapOut<'a>>,
}

pub fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Top-level folders Script Sync creates for services; anything else at the top is a Folder.
const SERVICES: &[&str] = &[
    "Workspace",
    "Players",
    "Lighting",
    "MaterialService",
    "ReplicatedFirst",
    "ReplicatedStorage",
    "ServerScriptService",
    "ServerStorage",
    "StarterGui",
    "StarterPack",
    "StarterPlayer",
    "StarterPlayerScripts",
    "StarterCharacterScripts",
    "SoundService",
    "Chat",
    "TextChatService",
    "Teams",
    "LocalizationService",
    "TestService",
];

fn is_script_file(name: &str) -> bool {
    name.ends_with(".luau") || name.ends_with(".lua")
}

/// Splits `Foo.server.luau` into (`Foo`, `Script`).
fn classify(file_name: &str) -> Option<(String, &'static str)> {
    let stem = file_name
        .strip_suffix(".luau")
        .or_else(|| file_name.strip_suffix(".lua"))?;
    for (suffix, class) in [
        (".server", "Script"),
        (".client", "Script"),
        (".local", "LocalScript"),
    ] {
        if let Some(name) = stem.strip_suffix(suffix) {
            return Some((name.to_string(), class));
        }
    }
    Some((stem.to_string(), "ModuleScript"))
}

impl Tree {
    fn empty(root_dir: PathBuf) -> Self {
        Tree {
            root_dir,
            nodes: vec![Node {
                name: "game".into(),
                class_name: "DataModel".into(),
                file: None,
                parent: None,
                children: Vec::new(),
            }],
            by_file: HashMap::new(),
            stale_files: Vec::new(),
            missing_files: Vec::new(),
        }
    }

    /// Loads `sourcemap.json` if present, otherwise scans the folder.
    pub fn load(root_dir: &Path, sourcemap: Option<&Path>) -> Result<Tree> {
        let root_dir = root_dir
            .canonicalize()
            .with_context(|| format!("root folder not found: {}", root_dir.display()))?;
        let sourcemap = sourcemap
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root_dir.join("sourcemap.json"));
        let tree = if sourcemap.is_file() {
            // Studio doesn't always refresh sourcemap.json, so scripts that exist
            // on disk but not in the map are merged in from the folder layout.
            let mut tree = Tree::from_sourcemap(root_dir.clone(), &sourcemap)?;
            let mut disk = Tree::from_filesystem(root_dir)?;
            // Both trees must have the same shape before merging, or the disk's
            // top-level StarterPlayerScripts would be added next to the nested one.
            tree.nest_starter_player();
            disk.nest_starter_player();
            tree.stale_files = tree.merge(ROOT, &disk, ROOT);
            tree.missing_files = tree
                .nodes
                .iter()
                .filter_map(|n| n.file.clone())
                .filter(|f| !tree.root_dir.join(f).is_file())
                .collect();
            tree
        } else {
            let mut tree = Tree::from_filesystem(root_dir)?;
            tree.nest_starter_player();
            tree
        };
        Ok(tree)
    }

    /// True when sourcemap.json and the folder disagree: scripts on disk it doesn't
    /// list, or listed scripts that are gone.
    pub fn is_stale(&self) -> bool {
        !self.stale_files.is_empty() || !self.missing_files.is_empty()
    }

    /// The merged tree in Studio's sourcemap.json format, without entries whose script
    /// file no longer exists.
    pub fn sourcemap_json(&self) -> String {
        fn node(tree: &Tree, id: NodeId) -> Option<SourcemapOut<'_>> {
            let n = &tree.nodes[id];
            let children: Vec<_> = n.children.iter().filter_map(|&c| node(tree, c)).collect();
            let file_exists = n
                .file
                .as_ref()
                .is_some_and(|f| tree.root_dir.join(f).is_file());
            if n.file.is_some() && !file_exists && children.is_empty() {
                return None;
            }
            Some(SourcemapOut {
                name: if id == ROOT { "Game" } else { &n.name },
                class_name: &n.class_name,
                file_paths: if file_exists {
                    n.file.iter().map(String::as_str).collect()
                } else {
                    Vec::new()
                },
                children,
            })
        }
        let value = node(self, ROOT).expect("root is never pruned");
        let mut text = serde_json::to_string_pretty(&value).expect("sourcemap serializes");
        text.push('\n');
        text
    }

    /// Writes `sourcemap_json` to `path`; returns false if it was already up to date.
    pub fn write_sourcemap(&self, path: &Path) -> Result<bool> {
        let text = self.sourcemap_json();
        if fs::read_to_string(path).is_ok_and(|old| old == text) {
            return Ok(false);
        }
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(true)
    }

    pub fn from_sourcemap(root_dir: PathBuf, path: &Path) -> Result<Tree> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let map: SourcemapNode =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut tree = Tree::empty(root_dir);
        for child in map.children {
            tree.insert_sourcemap(ROOT, child);
        }
        Ok(tree)
    }

    fn insert_sourcemap(&mut self, parent: NodeId, node: SourcemapNode) {
        let file = node
            .file_paths
            .iter()
            .find(|p| is_script_file(p))
            .map(|p| normalize(p));
        let id = self.add(parent, node.name, node.class_name, file);
        for child in node.children {
            self.insert_sourcemap(id, child);
        }
    }

    pub fn from_filesystem(root_dir: PathBuf) -> Result<Tree> {
        let mut tree = Tree::empty(root_dir.clone());
        tree.scan_dir(ROOT, &root_dir, "")?;
        Ok(tree)
    }

    fn scan_dir(&mut self, parent: NodeId, dir: &Path, rel: &str) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let path = entry.path();
            if path.is_dir() {
                // A folder holding init.* becomes that script; otherwise a Folder
                // (or the service itself at the top level).
                let init = fs::read_dir(&path)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("init.") && is_script_file(n))
                    .min();
                let (class, file) = match init.as_deref().and_then(classify) {
                    Some((_, class)) => (
                        class.to_string(),
                        Some(format!("{child_rel}/{}", init.unwrap())),
                    ),
                    None if parent == ROOT && SERVICES.contains(&name.as_str()) => {
                        (name.clone(), None)
                    }
                    None => ("Folder".to_string(), None),
                };
                let id = self.add(parent, name, class, file);
                self.scan_dir(id, &path, &child_rel)?;
            } else if is_script_file(&name) {
                if name.starts_with("init.") && parent != ROOT {
                    continue;
                }
                let Some((inst_name, class)) = classify(&name) else {
                    continue;
                };
                self.add(parent, inst_name, class.to_string(), Some(child_rel));
            }
        }
        Ok(())
    }

    /// Copies scripts from `other` that this tree lacks; returns their files.
    fn merge(&mut self, into: NodeId, other: &Tree, from: NodeId) -> Vec<String> {
        let mut added = Vec::new();
        for &oc in &other.nodes[from].children {
            let o = &other.nodes[oc];
            if o.file
                .as_ref()
                .is_some_and(|f| self.by_file.contains_key(f))
            {
                continue;
            }
            match self.child(into, &o.name) {
                Some(existing) => {
                    if self.nodes[existing].file.is_none()
                        && let Some(f) = &o.file
                    {
                        self.nodes[existing].file = Some(f.clone());
                        self.nodes[existing].class_name = o.class_name.clone();
                        self.by_file.insert(f.clone(), existing);
                        added.push(f.clone());
                    }
                    added.extend(self.merge(existing, other, oc));
                }
                None => {
                    let id = self.add(into, o.name.clone(), o.class_name.clone(), o.file.clone());
                    added.extend(o.file.clone());
                    added.extend(self.merge(id, other, oc));
                }
            }
        }
        added
    }

    /// Script Sync mirrors StarterPlayerScripts/StarterCharacterScripts at the top
    /// level, but in the DataModel they live under StarterPlayer.
    fn nest_starter_player(&mut self) {
        let movable: Vec<NodeId> = self.nodes[ROOT]
            .children
            .iter()
            .copied()
            .filter(|&c| {
                matches!(
                    self.nodes[c].name.as_str(),
                    "StarterPlayerScripts" | "StarterCharacterScripts"
                )
            })
            .collect();
        if movable.is_empty() {
            return;
        }
        let starter_player = match self.child(ROOT, "StarterPlayer") {
            Some(id) => id,
            None => self.add(ROOT, "StarterPlayer".into(), "StarterPlayer".into(), None),
        };
        self.nodes[ROOT].children.retain(|c| !movable.contains(c));
        for id in movable {
            self.nodes[id].parent = Some(starter_player);
            self.nodes[starter_player].children.push(id);
        }
    }

    fn add(
        &mut self,
        parent: NodeId,
        name: String,
        class_name: String,
        file: Option<String>,
    ) -> NodeId {
        let id = self.nodes.len();
        if let Some(f) = &file {
            self.by_file.insert(f.clone(), id);
        }
        self.nodes.push(Node {
            name,
            class_name,
            file,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.nodes[parent].children.push(id);
        id
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn child(&self, id: NodeId, name: &str) -> Option<NodeId> {
        self.nodes[id]
            .children
            .iter()
            .copied()
            .find(|&c| self.nodes[c].name == name)
    }

    pub fn ancestor_named(&self, id: NodeId, name: &str) -> Option<NodeId> {
        let mut cur = self.nodes[id].parent;
        while let Some(c) = cur {
            if self.nodes[c].name == name {
                return Some(c);
            }
            cur = self.nodes[c].parent;
        }
        None
    }

    pub fn by_file(&self, rel: &str) -> Option<NodeId> {
        self.by_file.get(&normalize(rel)).copied()
    }

    /// Finds the node for a file given relative to the root or as an absolute path.
    pub fn find_file(&self, path: &Path) -> Result<NodeId> {
        let rel = if path.is_absolute() {
            let abs = path
                .canonicalize()
                .with_context(|| format!("file not found: {}", path.display()))?;
            abs.strip_prefix(&self.root_dir)
                .with_context(|| {
                    format!("{} is outside {}", path.display(), self.root_dir.display())
                })?
                .to_string_lossy()
                .into_owned()
        } else {
            path.to_string_lossy().into_owned()
        };
        match self.by_file(&rel) {
            Some(id) => Ok(id),
            None => bail!("{rel} is not a script in the sync tree (is sourcemap.json stale?)"),
        }
    }

    /// Instance path segments from (but excluding) `game`.
    pub fn segments(&self, id: NodeId) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == ROOT {
                break;
            }
            out.push(self.nodes[c].name.clone());
            cur = self.nodes[c].parent;
        }
        out.reverse();
        out
    }

    pub fn full_name(&self, id: NodeId) -> String {
        if id == ROOT {
            return "game".into();
        }
        self.segments(id).join(".")
    }

    pub fn abs_file(&self, rel: &str) -> PathBuf {
        self.root_dir.join(rel)
    }
}
