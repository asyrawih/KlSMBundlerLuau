//! Writes a finished bundle: the `.luau`, its source map and optionally a `.rbxmx`.

use crate::bundle::Bundle;
use crate::tree::{Node, RunContext};
use anyhow::{Context, Result, bail};
use rbx_dom_weak::{InstanceBuilder, WeakDom};
use rbx_types::{Attributes, Enum, Ref};
use std::fs;
use std::path::{Path, PathBuf};

pub fn map_path(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".map.json");
    output.with_file_name(name)
}

fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Skip identical writes so Script Sync / watchers don't see spurious changes.
    if fs::read_to_string(path).is_ok_and(|old| old == contents) {
        return Ok(());
    }
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

pub fn write_bundle(bundle: &Bundle, output: &Path) -> Result<()> {
    write(output, &bundle.code)?;
    write(
        &map_path(output),
        &serde_json::to_string_pretty(&bundle.map)?,
    )
}

/// One instance in a Roblox XML model.
pub struct Item {
    pub class: String,
    pub name: String,
    /// Scripts: their source. `None` for folders.
    pub source: Option<String>,
    /// `Script` RunContext: 0 Legacy, 1 Server, 2 Client.
    pub run_context: Option<u8>,
    /// Boolean attributes set to true (`KlsmExact`).
    pub flags: Vec<String>,
    pub children: Vec<Item>,
}

impl Item {
    pub fn folder(name: &str) -> Item {
        Item {
            class: "Folder".into(),
            name: name.into(),
            source: None,
            run_context: None,
            flags: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Converts `item` and its children into `dom` under `parent`; returns the new instance.
fn insert_item(dom: &mut WeakDom, parent: Ref, item: &Item) -> Ref {
    let mut builder = InstanceBuilder::new(item.class.as_str()).with_name(&item.name);
    if let Some(source) = &item.source {
        builder = builder.with_property("Source", source.clone());
    }
    if let Some(rc) = item.run_context {
        builder = builder.with_property("RunContext", Enum::from_u32(rc as u32));
    }
    if !item.flags.is_empty() {
        let attributes = item
            .flags
            .iter()
            .fold(Attributes::new(), |a, flag| a.with(flag.as_str(), true));
        builder = builder.with_property("Attributes", attributes);
    }
    let id = dom.insert(parent, builder);
    for child in &item.children {
        insert_item(dom, id, child);
    }
    id
}

/// A model holding `roots`, ready for `add_assets` and `write_dom`.
pub fn model(roots: &[Item]) -> WeakDom {
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    for item in roots {
        insert_item(&mut dom, root, item);
    }
    dom
}

/// The child of `parent` named `name`, created as a Folder when missing.
pub fn folder(dom: &mut WeakDom, parent: Ref, name: &str) -> Ref {
    let existing = dom
        .get_by_ref(parent)
        .unwrap()
        .children()
        .iter()
        .copied()
        .find(|&c| {
            let child = dom.get_by_ref(c).unwrap();
            child.name == name && child.class == "Folder"
        });
    existing.unwrap_or_else(|| dom.insert(parent, InstanceBuilder::new("Folder").with_name(name)))
}

/// Adds every `.rbxm` / `.rbxmx` under `dir` to `dom` below `parent`, at the folder path the
/// file sits in: `assets/ReplicatedStorage/EffectDonation.rbxm` (a Studio "Save to File" of
/// `ReplicatedStorage.EffectDonation`) lands in `<parent>/ReplicatedStorage/`.
pub fn add_assets(dom: &mut WeakDom, parent: Ref, dir: &Path) -> Result<usize> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e == "rbxm" || e == "rbxmx")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    for file in &files {
        let reader = std::io::BufReader::new(fs::File::open(file)?);
        let mut asset = if file.extension().is_some_and(|e| e == "rbxm") {
            rbx_binary::from_reader(reader).map_err(anyhow::Error::from)
        } else {
            rbx_xml::from_reader_default(reader).map_err(anyhow::Error::from)
        }
        .with_context(|| format!("reading {}", file.display()))?;
        let mut into = parent;
        let rel = file
            .parent()
            .unwrap()
            .strip_prefix(dir)
            .unwrap_or(Path::new(""));
        if rel.as_os_str().is_empty() {
            bail!(
                "{}: put assets in a folder named after their service (e.g. {}/ReplicatedStorage/)",
                file.display(),
                dir.display()
            );
        }
        for segment in rel.iter() {
            into = folder(dom, into, &segment.to_string_lossy());
        }
        for child in asset.root().children().to_vec() {
            asset.transfer(child, dom, into);
        }
    }
    Ok(files.len())
}

/// Writes `dom`'s top-level instances as a Roblox XML model (drag into Studio).
pub fn write_dom(dom: &WeakDom, path: &Path) -> Result<()> {
    let mut xml = Vec::new();
    rbx_xml::to_writer_default(&mut xml, dom, dom.root().children())?;
    write(path, &String::from_utf8(xml)?)
}

/// A Roblox XML model holding `roots`.
pub fn write_model(roots: &[Item], path: &Path) -> Result<()> {
    write_dom(&model(roots), path)
}

/// The script Roblox needs to run a bundle whose entry was `entry`.
pub fn script_item(bundle_code: &str, entry: &Node, name: &str) -> Item {
    let (class, run_context) = if entry.is_module() {
        ("ModuleScript", None)
    } else if entry.class_name == "LocalScript" {
        ("LocalScript", None)
    } else {
        let rc = match entry.run_context() {
            RunContext::Legacy => 0,
            RunContext::Server => 1,
            RunContext::Client => 2,
        };
        ("Script", Some(rc))
    };
    Item {
        class: class.into(),
        name: name.into(),
        source: Some(bundle_code.to_string()),
        run_context,
        flags: Vec::new(),
        children: Vec::new(),
    }
}

/// A one-instance model of the bundle.
pub fn write_rbxmx(bundle: &Bundle, entry: &Node, name: &str, path: &Path) -> Result<()> {
    write_model(&[script_item(&bundle.code, entry, name)], path)
}
