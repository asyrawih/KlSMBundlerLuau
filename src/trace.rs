//! Maps bundle line numbers in Roblox error output back to the original files.

use crate::bundle::SourceMap;
use anyhow::{Context, Result};
use regex::{Captures, Regex};
use std::fs;
use std::path::Path;

pub fn load(path: &Path) -> Result<SourceMap> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn describe(map: &SourceMap, line: usize) -> String {
    match map.lookup(line) {
        Some((seg, 0)) => format!("{} ({})", seg.file, seg.instance),
        Some((seg, l)) => format!("{}:{l}", seg.file),
        None => format!("{}:{line} (bundle runtime)", map.script_name),
    }
}

/// Rewrites `...<ScriptName>:<line>` and Roblox's `Script '<...ScriptName>', Line <n>`
/// in a pasted error/stack trace.
pub fn rewrite(map: &SourceMap, text: &str) -> String {
    let name = regex::escape(&map.script_name);
    let stack =
        Regex::new(&format!(r"Script '(?:[\w.]+\.)?{name}', Line (\d+)")).expect("valid regex");
    let inline = Regex::new(&format!(r"(?:[\w.]+\.)?{name}:(\d+)")).expect("valid regex");
    let line = |caps: &Captures| caps[1].parse::<usize>().unwrap_or(0);
    let text = stack.replace_all(text, |caps: &Captures| match map.lookup(line(caps)) {
        Some((seg, l)) if l > 0 => format!("Script '{}', Line {l}", seg.file),
        _ => caps[0].to_string(),
    });
    inline
        .replace_all(&text, |caps: &Captures| describe(map, line(caps)))
        .into_owned()
}
