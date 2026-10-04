//! Writes a finished bundle: the `.luau`, its source map and optionally a `.rbxmx`.

use crate::bundle::Bundle;
use crate::tree::{Node, RunContext};
use anyhow::{Context, Result};
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

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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

/// Roblox's AttributesSerialize blob: count, then (name, type 0x03 bool, value) per entry.
fn bool_attributes(names: &[String]) -> String {
    let mut bytes = (names.len() as u32).to_le_bytes().to_vec();
    for name in names {
        bytes.extend((name.len() as u32).to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend([0x03, 0x01]);
    }
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ABC[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

fn push_item(out: &mut String, item: &Item, next_ref: &mut usize, depth: usize) {
    let pad = "  ".repeat(depth);
    out.push_str(&format!(
        "{pad}<Item class=\"{}\" referent=\"RBXKLSM{}\">\n{pad}  <Properties>\n{pad}    <string name=\"Name\">{}</string>\n",
        xml_escape(&item.class),
        next_ref,
        xml_escape(&item.name)
    ));
    *next_ref += 1;
    if let Some(rc) = item.run_context {
        out.push_str(&format!(
            "{pad}    <token name=\"RunContext\">{rc}</token>\n"
        ));
    }
    if !item.flags.is_empty() {
        out.push_str(&format!(
            "{pad}    <BinaryString name=\"AttributesSerialize\">{}</BinaryString>\n",
            bool_attributes(&item.flags)
        ));
    }
    if let Some(source) = &item.source {
        out.push_str(&format!(
            "{pad}    <ProtectedString name=\"Source\"><![CDATA[{}]]></ProtectedString>\n",
            source.replace("]]>", "]]]]><![CDATA[>")
        ));
    }
    out.push_str(&format!("{pad}  </Properties>\n"));
    for child in &item.children {
        push_item(out, child, next_ref, depth + 1);
    }
    out.push_str(&format!("{pad}</Item>\n"));
}

/// A Roblox XML model holding `roots`: drag into Studio or insert with a plugin.
pub fn write_model(roots: &[Item], path: &Path) -> Result<()> {
    let mut xml = String::from(
        "<roblox xmlns:xmime=\"http://www.w3.org/2005/05/xmlmime\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"http://www.roblox.com/roblox.xsd\" version=\"4\">\n",
    );
    let mut next_ref = 0;
    for root in roots {
        push_item(&mut xml, root, &mut next_ref, 1);
    }
    xml.push_str("</roblox>\n");
    write(path, &xml)
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
