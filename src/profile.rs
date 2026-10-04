//! Per-client profiles: `clients/<name>.toml` next to bundle.toml lists the features that
//! client doesn't get. Building with a profile excludes their folders from every bundle and
//! writes the outputs to `dist/<name>/` instead of the configured paths.

use crate::config::Config;
use crate::output::{Item, script_item, write_model};
use crate::tree::{NodeId, ROOT, RunContext, Tree};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Feature folder names left out of this client's bundles.
    #[serde(default)]
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Feature {
    pub name: String,
    /// The folders it has code in, named by the wildcard part of `features` (`Server`, `Shared`).
    pub sides: Vec<String>,
}

pub fn clients_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("clients")
}

fn profile_path(config_path: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("client name {name:?} may only use letters, digits, - and _");
    }
    Ok(clients_dir(config_path).join(format!("{name}.toml")))
}

/// Client names, sorted; none when `clients/` doesn't exist yet.
pub fn list(config_path: &Path) -> Result<Vec<String>> {
    let Ok(entries) = fs::read_dir(clients_dir(config_path)) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".toml").map(str::to_string)
        })
        .collect();
    names.sort();
    Ok(names)
}

pub fn load(config_path: &Path, name: &str) -> Result<Profile> {
    let path = profile_path(config_path, name)?;
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn save(config_path: &Path, name: &str, profile: &Profile) -> Result<()> {
    let path = profile_path(config_path, name)?;
    fs::create_dir_all(clients_dir(config_path))?;
    let mut profile = profile.clone();
    profile.disabled.sort();
    profile.disabled.dedup();
    fs::write(&path, toml::to_string_pretty(&profile)?)
        .with_context(|| format!("writing {}", path.display()))
}

fn subdirs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Folders matched by `config.features` (`Addon/*/Features`) as (label, path from root); the
/// label joins the parts the wildcards matched.
fn feature_dirs(config: &Config) -> Result<Vec<(String, String)>> {
    let Some(pattern) = &config.features else {
        bail!("set `features` in bundle.toml (e.g. features = \"Addon/*/Features\")");
    };
    let mut found = vec![(Vec::<String>::new(), String::new())];
    for segment in pattern.split('/').filter(|s| !s.is_empty()) {
        let wild = segment.contains(['*', '?', '[']);
        let glob = globset::Glob::new(segment)?.compile_matcher();
        found = found
            .into_iter()
            .flat_map(|(label, rel)| {
                let names = if wild {
                    subdirs(&config.root.join(&rel))
                        .into_iter()
                        .filter(|n| glob.is_match(n))
                        .collect()
                } else {
                    vec![segment.to_string()]
                };
                names.into_iter().map(move |name| {
                    let mut label = label.clone();
                    if wild {
                        label.push(name.clone());
                    }
                    let rel = if rel.is_empty() {
                        name
                    } else {
                        format!("{rel}/{name}")
                    };
                    (label, rel)
                })
            })
            .filter(|(_, rel)| config.root.join(rel).is_dir())
            .collect();
    }
    Ok(found
        .into_iter()
        .map(|(label, rel)| (label.join("/"), rel))
        .collect())
}

/// Every feature folder, by name, with the sides it appears in.
pub fn features(config: &Config) -> Result<Vec<Feature>> {
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (label, rel) in feature_dirs(config)? {
        for name in subdirs(&config.root.join(&rel)) {
            by_name.entry(name).or_default().push(label.clone());
        }
    }
    Ok(by_name
        .into_iter()
        .map(|(name, sides)| Feature { name, sides })
        .collect())
}

const INSTALLER: &str = include_str!("installer.luau");

/// Client-side entries: LocalScripts and `.client` Scripts.
fn is_client(tree: &Tree, id: NodeId) -> bool {
    let node = tree.node(id);
    node.class_name == "LocalScript" || node.run_context() == RunContext::Client
}

fn dist_dir(config_path: &Path, name: &str) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("dist")
        .join(name)
}

/// Points `config` at client `name`: its disabled features are excluded from every bundle,
/// outputs go to `dist/<name>/`, and server bundles start with the package installer (see
/// `write_package`, which replaces the per-bundle `.rbxmx` files).
pub fn apply(config: &mut Config, config_path: &Path, name: &str, profile: &Profile) -> Result<()> {
    let dirs = feature_dirs(config)?;
    let mut excludes = Vec::new();
    for feature in &profile.disabled {
        for (_, rel) in &dirs {
            let dir = format!("{rel}/{feature}");
            if config.root.join(&dir).is_dir() {
                excludes.push(format!("{dir}/**"));
            }
        }
    }
    let tree = Tree::load(&config.root, config.sourcemap.as_deref())?;
    let dist = dist_dir(config_path, name);
    for target in &mut config.bundles {
        target.exclude.extend(excludes.iter().cloned());
        target.output = dist.join(target.output.file_name().unwrap_or_default());
        target.rbxmx = None;
        if !is_client(&tree, tree.find_file(&target.entry)?) {
            target.prologue = Some(INSTALLER.to_string());
        }
    }
    Ok(())
}

/// The model item for `id` and what's under it: ModuleScripts with their source and the
/// folders leading to them, minus `excluded` files; `None` when nothing is left.
fn module_item(
    tree: &Tree,
    id: NodeId,
    excluded: &globset::GlobSet,
    exact: &[NodeId],
) -> Result<Option<Item>> {
    let node = tree.node(id);
    let mut children = Vec::new();
    for &child in &node.children {
        children.extend(module_item(tree, child, excluded, exact)?);
    }
    let mut item = match &node.file {
        Some(file) if node.is_module() => {
            if excluded.is_match(file) {
                return Ok(None);
            }
            let source = fs::read_to_string(tree.abs_file(file))
                .with_context(|| format!("reading {file}"))?;
            Item {
                class: "ModuleScript".into(),
                source: Some(source.replace("\r\n", "\n")),
                ..Item::folder(&node.name)
            }
        }
        Some(_) => return Ok(None),
        // An exact folder ships even empty, or the installer couldn't clear the old one.
        None if children.is_empty() && !exact.contains(&id) => return Ok(None),
        None => Item::folder(&node.name),
    };
    item.children = children;
    if exact.contains(&id) {
        item.flags.push("KlsmExact".into());
    }
    Ok(Some(item))
}

/// Adds `item` under `parent`, merging folders that already exist by name.
fn merge_into(parent: &mut Item, item: Item) {
    match parent
        .children
        .iter_mut()
        .find(|c| c.name == item.name && c.class == "Folder" && item.class == "Folder")
    {
        Some(existing) => {
            existing.flags.extend(item.flags);
            for child in item.children {
                merge_into(existing, child);
            }
        }
        None => parent.children.push(item),
    }
}

/// Writes `dist/<name>/<name>.rbxmx`, one model to drop into ServerScriptService: the server
/// bundles, plus a folder per service holding the client bundles (as RunContext Client
/// Scripts) and every module under `external` this client gets. The server bundle's installer
/// moves those folders into place when the server starts. Call after a successful build.
pub fn write_package(config: &Config, config_path: &Path, name: &str) -> Result<PathBuf> {
    let tree = Tree::load(&config.root, config.sourcemap.as_deref())?;
    let mut package = Item::folder("KlsmPackage");

    let mut excluded = globset::GlobSetBuilder::new();
    for pattern in config.bundles.iter().flat_map(|b| &b.exclude) {
        excluded.add(
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()?,
        );
    }
    let excluded = excluded.build()?;
    let external: Vec<String> = config
        .external
        .iter()
        .chain(config.bundles.iter().flat_map(|b| &b.external))
        .cloned()
        .collect();

    // Feature folders under `external` lose switched-off features when installed.
    let mut exact = Vec::new();
    for (_, rel) in feature_dirs(config)? {
        let prefix = format!("{rel}/");
        let leaf = rel.rsplit('/').next().unwrap_or_default();
        if let Some(mut id) = tree
            .nodes
            .iter()
            .position(|n| n.file.as_ref().is_some_and(|f| f.starts_with(&prefix)))
            .filter(|&id| tree.is_under(id, &external))
        {
            while tree.node(id).name != leaf {
                match tree.node(id).parent {
                    Some(p) => id = p,
                    None => break,
                }
            }
            exact.push(id);
        }
    }

    for path in &external {
        let segments: Vec<&str> = path
            .split(['.', '/'])
            .filter(|s| !s.is_empty() && *s != "game")
            .collect();
        let mut id = ROOT;
        for segment in &segments {
            match tree.child(id, segment) {
                Some(c) => id = c,
                None => bail!("external path {path} is not in the tree"),
            }
        }
        let Some(mut item) = module_item(&tree, id, &excluded, &exact)? else {
            continue;
        };
        // Wrap in folders for the path above it, up to the service.
        for segment in segments[..segments.len() - 1].iter().rev() {
            let mut folder = Item::folder(segment);
            folder.children.push(item);
            item = folder;
        }
        merge_into(&mut package, item);
    }

    for target in &config.bundles {
        let entry = tree.find_file(&target.entry)?;
        let code = fs::read_to_string(&target.output)
            .with_context(|| format!("reading {}", target.output.display()))?;
        let mut script = script_item(&code, tree.node(entry), &target.script_name());
        if is_client(&tree, entry) {
            script.class = "Script".into();
            script.run_context = Some(2);
            let mut rs = Item::folder("ReplicatedStorage");
            rs.children.push(script);
            merge_into(&mut package, rs);
        } else {
            package.children.insert(0, script);
        }
    }

    let dist = dist_dir(config_path, name);
    // Older builds left one model per bundle; the package replaces them.
    for entry in fs::read_dir(&dist).into_iter().flatten().flatten() {
        if entry.path().extension().is_some_and(|e| e == "rbxmx") {
            fs::remove_file(entry.path())?;
        }
    }
    let path = dist.join(format!("{name}.rbxmx"));
    write_model(&[package], &path)?;
    Ok(path)
}
