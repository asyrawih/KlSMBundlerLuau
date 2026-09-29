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

/// A one-instance Roblox XML model: drag into Studio or insert with a plugin.
pub fn write_rbxmx(bundle: &Bundle, entry: &Node, name: &str, path: &Path) -> Result<()> {
    let class = if entry.is_module() {
        "ModuleScript"
    } else {
        entry.class_name.as_str()
    };
    let run_context = if class == "Script" {
        let value = match entry.run_context() {
            RunContext::Legacy => 0,
            RunContext::Server => 1,
            RunContext::Client => 2,
        };
        format!("\n      <token name=\"RunContext\">{value}</token>")
    } else {
        String::new()
    };
    let source = bundle.code.replace("]]>", "]]]]><![CDATA[>");
    let xml = format!(
        r#"<roblox xmlns:xmime="http://www.w3.org/2005/05/xmlmime" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="http://www.roblox.com/roblox.xsd" version="4">
  <Item class="{class}" referent="RBXKLSM0">
    <Properties>
      <string name="Name">{}</string>{run_context}
      <ProtectedString name="Source"><![CDATA[{source}]]></ProtectedString>
    </Properties>
  </Item>
</roblox>
"#,
        xml_escape(name)
    );
    write(path, &xml)
}
