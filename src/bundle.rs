//! Walks the require graph from an entry script and emits one self-contained file.

use crate::analyze::{self, Aliases, Analysis, Target};
use crate::minify::{Minify, minify};
use crate::tree::{NodeId, Tree};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;

const RUNTIME: &str = include_str!("runtime.luau");
const PROXY_MARKER: &str = "--@@SCRIPT_PROXY@@\n";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub level: Level,
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let level = match self.level {
            Level::Warning => "warning",
            Level::Error => "error",
        };
        if self.line > 0 {
            write!(f, "{level}: {}:{}: {}", self.file, self.line, self.message)
        } else {
            write!(f, "{level}: {}: {}", self.file, self.message)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub file: String,
    pub instance: String,
    /// First and last output line of the module body (1-based, inclusive).
    pub out_start: usize,
    pub out_end: usize,
    /// False when minification merged lines, so only the module is known.
    pub exact: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMap {
    pub version: u32,
    pub script_name: String,
    pub entry: String,
    pub segments: Vec<Segment>,
}

impl SourceMap {
    /// Maps an output line back to `(file, line)`; `line` is 0 when inexact.
    pub fn lookup(&self, line: usize) -> Option<(&Segment, usize)> {
        let seg = self
            .segments
            .iter()
            .find(|s| (s.out_start..=s.out_end).contains(&line))?;
        Some((
            seg,
            if seg.exact {
                line - seg.out_start + 1
            } else {
                0
            },
        ))
    }
}

#[derive(Debug)]
pub struct Bundle {
    pub code: String,
    pub map: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
    pub module_count: usize,
    /// Every source file that went in, for watch mode.
    pub inputs: Vec<String>,
}

impl Bundle {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.level == Level::Error)
    }
}

struct Unit {
    node: NodeId,
    file: String,
    source: String,
    analysis: Analysis,
    /// Module id; the entry script has none unless it is itself a ModuleScript.
    id: Option<usize>,
    deps: Vec<NodeId>,
}

pub struct Options<'a> {
    pub tree: &'a Tree,
    pub aliases: &'a Aliases,
    pub entry: NodeId,
    pub minify: Minify,
    pub script_name: String,
}

pub fn bundle(opts: &Options) -> Bundle {
    let tree = opts.tree;
    let mut diags = Vec::new();
    let entry = tree.node(opts.entry);
    let entry_file = entry.file.clone().unwrap_or_default();
    if !entry.is_module() && !entry.is_runnable() {
        diags.push(Diagnostic {
            level: Level::Error,
            file: entry_file.clone(),
            line: 0,
            message: format!(
                "entry must be a Script, LocalScript or ModuleScript, found {}",
                entry.class_name
            ),
        });
    }

    // BFS over the require graph. Unit index 0 is the entry; module ids are the unit
    // index, shifted by one when the entry is itself a ModuleScript (id 0 is unused).
    let mut units: Vec<Unit> = Vec::new();
    let mut order: Vec<NodeId> = vec![opts.entry];
    let mut seen: HashMap<NodeId, usize> = HashMap::from([(opts.entry, 0)]);
    let id_shift = usize::from(entry.is_module());

    while units.len() < order.len() {
        let index = units.len();
        let node = order[index];
        let file = tree.node(node).file.clone().unwrap_or_default();
        let id = (index > 0 || entry.is_module()).then_some(index + id_shift);
        let mut unit = Unit {
            node,
            file: file.clone(),
            source: String::new(),
            analysis: Analysis::default(),
            id,
            deps: vec![],
        };
        match fs::read_to_string(tree.abs_file(&file)) {
            Ok(s) => unit.source = s.replace("\r\n", "\n"),
            Err(e) => {
                diags.push(error(&file, 0, format!("cannot read file: {e}")));
                units.push(unit);
                continue;
            }
        }
        match analyze::analyze(&unit.source, tree, node, opts.aliases) {
            Ok(a) => unit.analysis = a,
            Err(errors) => {
                for e in errors {
                    diags.push(error(&file, e.line, e.message));
                }
                units.push(unit);
                continue;
            }
        }
        analyze::attach_text(&unit.source, &mut unit.analysis);

        for site in unit.analysis.requires.iter().filter(|r| !r.never_runs) {
            match &site.target {
                Target::Node(target) => {
                    let t = tree.node(*target);
                    if !t.is_module() {
                        diags.push(error(
                            &file,
                            site.line,
                            format!(
                                "{} resolves to {} {}; only ModuleScripts can be required",
                                site.text,
                                t.class_name,
                                tree.full_name(*target)
                            ),
                        ));
                    } else if t.file.is_none() {
                        diags.push(error(
                            &file,
                            site.line,
                            format!(
                                "{} resolves to {}, which has no synced source file",
                                site.text,
                                tree.full_name(*target)
                            ),
                        ));
                    } else {
                        seen.entry(*target).or_insert_with(|| {
                            order.push(*target);
                            order.len() - 1
                        });
                        unit.deps.push(*target);
                    }
                }
                // Roblox only fails a bad require when it runs, and guarded fallbacks
                // (`game and Enum or require("../test/mock")`) never do.
                Target::Missing(why) => diags.push(warning(
                    &file,
                    site.line,
                    format!(
                        "cannot resolve {}: {why}; left as a real require (errors if it runs)",
                        site.text
                    ),
                )),
                Target::Dynamic => diags.push(warning(
                    &file,
                    site.line,
                    format!(
                        "{} is not statically resolvable; left as a real require",
                        site.text
                    ),
                )),
                Target::Asset => {}
            }
        }
        if !unit.analysis.script_uses.is_empty() {
            let lines = unit
                .analysis
                .script_uses
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let message = if id.is_some() {
                format!(
                    "`script` used at runtime (lines {lines}); in the bundle it is a stand-in that only knows its Name, Parent and child paths"
                )
            } else {
                format!(
                    "`script` used at runtime (lines {lines}); in the bundle it is the bundle Script, not {}",
                    tree.full_name(node)
                )
            };
            diags.push(warning(&file, unit.analysis.script_uses[0], message));
        }
        units.push(unit);
    }

    let id_of: HashMap<NodeId, usize> = units
        .iter()
        .filter_map(|u| u.id.map(|id| (u.node, id)))
        .collect();
    report_cycles(&units, tree, &id_of, &mut diags);

    let (code, segments) = emit(&units, opts, &id_of, &mut diags);
    diags.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    Bundle {
        code,
        map: SourceMap {
            version: 1,
            script_name: opts.script_name.clone(),
            entry: entry_file,
            segments,
        },
        diagnostics: diags,
        module_count: id_of.len(),
        inputs: units.iter().map(|u| u.file.clone()).collect(),
    }
}

fn error(file: &str, line: usize, message: String) -> Diagnostic {
    Diagnostic {
        level: Level::Error,
        file: file.to_string(),
        line,
        message,
    }
}

fn warning(file: &str, line: usize, message: String) -> Diagnostic {
    Diagnostic {
        level: Level::Warning,
        file: file.to_string(),
        line,
        message,
    }
}

/// Static cycles are legal when at least one require is lazy (inside a function),
/// so they are warnings; the runtime raises Roblox's own error if one actually loops.
fn report_cycles(
    units: &[Unit],
    tree: &Tree,
    id_of: &HashMap<NodeId, usize>,
    diags: &mut Vec<Diagnostic>,
) {
    let by_id: HashMap<usize, &Unit> = units
        .iter()
        .filter_map(|u| u.id.map(|id| (id, u)))
        .collect();
    let mut state: HashMap<usize, u8> = HashMap::new(); // 1 = visiting, 2 = done
    let mut stack: Vec<usize> = Vec::new();
    fn visit(
        id: usize,
        by_id: &HashMap<usize, &Unit>,
        id_of: &HashMap<NodeId, usize>,
        state: &mut HashMap<usize, u8>,
        stack: &mut Vec<usize>,
        found: &mut Vec<Vec<usize>>,
    ) {
        state.insert(id, 1);
        stack.push(id);
        for dep in by_id[&id].deps.iter().filter_map(|n| id_of.get(n).copied()) {
            match state.get(&dep) {
                Some(1) => {
                    let start = stack.iter().position(|&s| s == dep).unwrap();
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(dep);
                    found.push(cycle);
                }
                Some(_) => {}
                None => visit(dep, by_id, id_of, state, stack, found),
            }
        }
        stack.pop();
        state.insert(id, 2);
    }
    let mut found = Vec::new();
    let mut ids: Vec<usize> = by_id.keys().copied().collect();
    ids.sort();
    for id in ids {
        if !state.contains_key(&id) {
            visit(id, &by_id, id_of, &mut state, &mut stack, &mut found);
        }
    }
    for cycle in found {
        let names: Vec<String> = cycle
            .iter()
            .map(|id| tree.node(by_id[id].node).name.clone())
            .collect();
        diags.push(warning(
            &by_id[&cycle[0]].file,
            0,
            format!(
                "circular require {}; fine only if one of these requires runs lazily",
                names.join(" -> ")
            ),
        ));
    }
}

fn lua_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 32 => out.push_str(&format!("\\{:03}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Rewrites resolved requires and strips `export` from type declarations, keeping
/// the line count of every replaced span so the source map stays exact.
fn rewrite(unit: &Unit, tree: &Tree, id_of: &HashMap<NodeId, usize>, annotate: bool) -> String {
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for site in unit.analysis.requires.iter().filter(|r| !r.never_runs) {
        let Target::Node(n) = site.target else {
            continue;
        };
        let Some(id) = id_of.get(&n) else { continue };
        let pad = "\n".repeat(site.text.matches('\n').count());
        let text = if annotate {
            format!(
                "__KLSM_require({id} --[[{}]]){pad}",
                tree.full_name(n).replace("]]", "] ]")
            )
        } else {
            format!("__KLSM_require({id}){pad}")
        };
        edits.push((site.range.clone(), text));
    }
    if unit.id.is_some() {
        for range in &unit.analysis.exports {
            edits.push((range.clone(), String::new()));
        }
    }
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    let mut out = unit.source.clone();
    for (range, text) in edits {
        out.replace_range(range, &text);
    }
    out
}

fn emit(
    units: &[Unit],
    opts: &Options,
    id_of: &HashMap<NodeId, usize>,
    diags: &mut Vec<Diagnostic>,
) -> (String, Vec<Segment>) {
    let tree = opts.tree;
    let entry = &units[0];
    let mut out = String::new();
    let mut segments = Vec::new();
    let exact = opts.minify != Minify::Full;
    let mut squash = |text: &str, file: &str| match minify(text, opts.minify) {
        Ok(s) => s,
        Err(e) => {
            diags.push(error(file, 0, e.to_string()));
            text.to_string()
        }
    };

    out.push_str("--!nocheck\n");
    for d in &entry.analysis.directives {
        if d == "native" || d.starts_with("optimize") {
            out.push_str(&format!("--!{d}\n"));
        }
    }
    out.push_str(&format!(
        "-- Bundled by KlSMBundlerLuau {VERSION} from {} ({} modules). Generated file: edit the sources, not this.\n",
        entry.file,
        id_of.len()
    ));

    let needs_proxy = units
        .iter()
        .any(|u| u.id.is_some() && !u.analysis.script_uses.is_empty());
    let runtime = match RUNTIME.split_once(PROXY_MARKER) {
        Some((core, proxy)) if needs_proxy => format!("{core}{proxy}"),
        Some((core, _)) => core.to_string(),
        None => RUNTIME.to_string(),
    };
    out.push_str(&squash(&runtime, "<runtime>"));
    if !out.ends_with('\n') {
        out.push('\n');
    }

    let annotate = opts.minify == Minify::None;
    let mut modules: Vec<&Unit> = units.iter().filter(|u| u.id.is_some()).collect();
    modules.sort_by_key(|u| u.id);
    for unit in modules {
        let id = unit.id.unwrap();
        let body = squash(&rewrite(unit, tree, id_of, annotate), &unit.file);
        let proxy = if unit.analysis.script_uses.is_empty() {
            String::new()
        } else {
            let segs: Vec<String> = tree
                .segments(unit.node)
                .iter()
                .map(|s| lua_string(s))
                .collect();
            format!(" local script = __KLSM_script({{{}}})", segs.join(", "))
        };
        out.push_str(&format!(
            "__KLSM_names[{id}] = {}\n",
            lua_string(&tree.full_name(unit.node))
        ));
        let comment = if annotate {
            format!(" -- {}", unit.file)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "__KLSM_modules[{id}] = function(...){proxy}{comment}\n"
        ));
        push_body(&mut out, &mut segments, unit, tree, &body, exact);
        out.push_str("end\n");
    }

    if let Some(id) = entry.id {
        out.push_str(&format!("return __KLSM_require({id})\n"));
    } else {
        let body = squash(&rewrite(entry, tree, id_of, annotate), &entry.file);
        out.push_str(&format!("do -- {}\n", entry.file));
        push_body(&mut out, &mut segments, entry, tree, &body, exact);
        out.push_str("end\n");
    }
    (out, segments)
}

fn push_body(
    out: &mut String,
    segments: &mut Vec<Segment>,
    unit: &Unit,
    tree: &Tree,
    body: &str,
    exact: bool,
) {
    let out_start = out.matches('\n').count() + 1;
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    let out_end = out.matches('\n').count();
    segments.push(Segment {
        file: unit.file.clone(),
        instance: tree.full_name(unit.node),
        out_start,
        out_end: out_end.max(out_start),
        exact,
    });
}
