use full_moon::LuaVersion;
use full_moon::tokenizer::{Lexer, TokenType};
use klsm_bundler::analyze::Aliases;
use klsm_bundler::bundle::{self, Bundle, Level, Options};
use klsm_bundler::minify::Minify;
use klsm_bundler::trace;
use klsm_bundler::tree::{ROOT, Tree};
use std::path::Path;

fn build(fixture: &str, entry: &str, minify: Minify) -> Bundle {
    let tree = Tree::load(&Path::new("tests/fixtures").join(fixture), None).unwrap();
    let entry = tree.find_file(Path::new(entry)).unwrap();
    bundle::bundle(&Options {
        tree: &tree,
        aliases: &Aliases::new(),
        entry,
        minify,
        script_name: "Bundle".into(),
        external: &[],
        internal: &[],
        include: &[],
        exclude: &[],
        prologue: "",
    })
}

/// Whether `code` parses; on a big stack, since full-moon's parser is recursive and debug
/// builds overflow the test threads' default stack on bundles that carry the full runtime.
fn parses(code: &str) -> bool {
    let code = code.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            full_moon::parse_fallible(&code, LuaVersion::luau())
                .errors()
                .is_empty()
        })
        .unwrap()
        .join()
        .unwrap()
}

fn diagnostics(b: &Bundle) -> String {
    b.diagnostics.iter().map(|d| format!("{d}\n")).collect()
}

/// Non-trivia tokens, ignoring the `--[[path]]` annotations the unminified output adds.
fn code_tokens(code: &str) -> Vec<String> {
    Lexer::new(code, LuaVersion::luau())
        .collect()
        .unwrap()
        .iter()
        .filter(|t| !t.token_type().is_trivia() && !matches!(t.token_type(), TokenType::Eof))
        .map(|t| t.to_string())
        .collect()
}

#[test]
fn module_entry_bundle() {
    let b = build("basic", "ReplicatedStorage/Shared/Suite.luau", Minify::None);
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    assert_eq!(b.module_count, 10);
    assert!(parses(&b.code));
    insta::assert_snapshot!("suite_diagnostics", diagnostics(&b));
    insta::assert_snapshot!("suite_code", b.code);
}

#[test]
fn script_entry_bundle() {
    let b = build(
        "basic",
        "ServerScriptService/Server/Main.server.luau",
        Minify::None,
    );
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    // Only what Main reaches: Greeter and Counter.
    assert_eq!(b.module_count, 2);
    assert!(
        b.code
            .contains("do -- ServerScriptService/Server/Main.server.luau")
    );
    insta::assert_snapshot!("main_code", b.code);
}

#[test]
fn type_only_requires_are_not_bundled() {
    let b = build("basic", "ReplicatedStorage/Shared/Suite.luau", Minify::None);
    assert!(b.code.contains("type G = typeof(require(Shared.Greeter))"));
}

#[test]
fn exports_are_stripped_inside_modules() {
    let b = build("basic", "ReplicatedStorage/Shared/Suite.luau", Minify::None);
    assert!(!b.code.contains("export type"));
}

#[test]
fn minified_output_has_identical_tokens() {
    let plain = build("basic", "ReplicatedStorage/Shared/Suite.luau", Minify::None);
    let plain_tokens = code_tokens(&plain.code);
    for level in [Minify::Light, Minify::Full] {
        let min = build("basic", "ReplicatedStorage/Shared/Suite.luau", level);
        assert!(min.code.len() < plain.code.len());
        assert!(parses(&min.code), "{level:?}");
        assert_eq!(code_tokens(&min.code), plain_tokens, "{level:?}");
    }
}

#[test]
fn light_minify_keeps_line_numbers() {
    let plain = build("basic", "ReplicatedStorage/Shared/Suite.luau", Minify::None);
    let light = build(
        "basic",
        "ReplicatedStorage/Shared/Suite.luau",
        Minify::Light,
    );
    let find = |b: &Bundle| {
        b.code
            .lines()
            .position(|l| l.contains("`hello {who}"))
            .unwrap()
            + 1
    };
    for b in [&plain, &light] {
        let (seg, line) = b.map.lookup(find(b)).unwrap();
        assert_eq!(
            (seg.file.as_str(), line),
            ("ReplicatedStorage/Shared/Greeter.luau", 13)
        );
    }
}

#[test]
fn trace_rewrites_error_lines() {
    let b = build("basic", "ReplicatedStorage/Shared/Suite.luau", Minify::None);
    let line = b
        .code
        .lines()
        .position(|l| l.contains("c.value += 1"))
        .unwrap()
        + 1;
    let log = format!("ServerScriptService.Bundle:{line}: attempt to index nil\n");
    assert_eq!(
        trace::rewrite(&b.map, &log),
        "ReplicatedStorage/Shared/Util/Counter.luau:11: attempt to index nil\n"
    );
}

#[test]
fn reports_errors_and_warnings() {
    let b = build(
        "errors",
        "ServerScriptService/Main.server.luau",
        Minify::None,
    );
    assert!(b.has_errors());
    let errors: Vec<_> = b
        .diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .collect();
    assert_eq!(errors.len(), 3);
    insta::assert_snapshot!("errors_diagnostics", diagnostics(&b));
}

#[test]
fn stale_sourcemap_is_patched_from_disk() {
    let tree = Tree::load(Path::new("tests/fixtures/stale"), None).unwrap();
    assert_eq!(
        tree.stale_files,
        vec!["ReplicatedStorage/New.luau".to_string()]
    );
    let b = build("stale", "ReplicatedStorage/Old.luau", Minify::None);
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    assert_eq!(b.module_count, 2);
}

/// Runs against a real Script Sync project: `KLSM_REAL_PROJECT=../RessoMusic cargo test real_project`.
#[test]
fn real_project_minifies_losslessly() {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(real_project)
        .unwrap()
        .join()
        .unwrap();
}

fn real_project() {
    let Ok(root) = std::env::var("KLSM_REAL_PROJECT") else {
        return;
    };
    let tree = Tree::load(Path::new(&root), None).unwrap();
    for entry in [
        "ServerScriptService/Server/Main.server.luau",
        "StarterPlayerScripts/Client/Main.local.luau",
        "StarterPlayerScripts/Client/Main.client.luau",
        "ServerScriptService/Server.server.luau",
        "StarterPlayerScripts/Client.local.luau",
    ] {
        // A stale sourcemap can still list a script that was moved or deleted.
        if !Path::new(&root).join(entry).is_file() {
            continue;
        }
        let Ok(entry) = tree.find_file(Path::new(entry)) else {
            continue;
        };
        let make = |minify| {
            bundle::bundle(&Options {
                tree: &tree,
                aliases: &Aliases::new(),
                entry,
                minify,
                script_name: "B".into(),
                external: &[],
                internal: &[],
                include: &[],
                exclude: &[],
                prologue: "",
            })
        };
        let plain = make(Minify::None);
        assert!(!plain.has_errors(), "{}", diagnostics(&plain));
        let tokens = code_tokens(&plain.code);
        for level in [Minify::Light, Minify::Full] {
            let min = make(level);
            assert!(parses(&min.code));
            assert_eq!(code_tokens(&min.code), tokens, "{level:?}");
            eprintln!(
                "{level:?}: {} KB -> {} KB",
                plain.code.len() / 1024,
                min.code.len() / 1024
            );
        }
        // Renaming changes tokens, so only check it parses and keeps Full's lines.
        let max = make(Minify::Max);
        assert!(!max.has_errors(), "{}", diagnostics(&max));
        assert!(parses(&max.code));
        assert_eq!(
            max.code.lines().count(),
            make(Minify::Full).code.lines().count()
        );
    }
}

/// Executes the Suite bundle with the Luau CLI: `LUAU_BIN=/path/to/luau cargo test runs_under_luau`.
#[test]
fn runs_under_luau() {
    let Ok(luau) = std::env::var("LUAU_BIN") else {
        return;
    };
    for level in [Minify::None, Minify::Light, Minify::Full, Minify::Max] {
        let b = build("basic", "ReplicatedStorage/Shared/Suite.luau", level);
        let script = format!(
            "game = {{ GetService = function() return {{ WaitForChild = function() return {{}} end }} end }}\n\
             local r = (function()\n{}\nend)()\n\
             print(r.greet, r.name, r.parentName, r.sameModule, r.helper, r.pkg, r.aPartner, r.bPartner, r.cached, r.brokenOk)\n\
             print(r.brokenErr)\n",
            b.code
        );
        let dir = std::env::temp_dir().join(format!("klsm-test-{}-{level:?}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("suite.luau");
        std::fs::write(&file, script).unwrap();
        let out = std::process::Command::new(&luau)
            .arg(&file)
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{level:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut lines = stdout.lines();
        assert_eq!(
            lines.next().unwrap(),
            "hello roblox #1\tGreeter\tShared\ttrue\t42\t1.0\tB\tA\ttrue\tfalse",
            "{level:?}"
        );
        assert!(lines.next().unwrap().ends_with(
            "Module code did not return exactly one value: ReplicatedStorage.Shared.Broken"
        ));
    }
}

#[test]
fn luaurc_aliases_resolve() {
    let root = Path::new("tests/fixtures/alias");
    let tree = Tree::load(root, None).unwrap();
    let aliases = klsm_bundler::config::load_aliases(&tree.root_dir).unwrap();
    let entry = tree
        .find_file(Path::new("ServerScriptService/Main.server.luau"))
        .unwrap();
    let b = bundle::bundle(&Options {
        tree: &tree,
        aliases: &aliases,
        entry,
        minify: Minify::None,
        script_name: "B".into(),
        external: &[],
        internal: &[],
        include: &[],
        exclude: &[],
        prologue: "",
    });
    assert!(
        !b.has_errors() && b.diagnostics.is_empty(),
        "{}",
        diagnostics(&b)
    );
    assert!(
        b.code
            .contains("print(__KLSM_require(1 --[[ReplicatedStorage.Shared.Util]]))")
    );
}

#[test]
fn game_guarded_fallbacks_are_ignored() {
    let b = build("guard", "ReplicatedStorage/Lib.luau", Minify::None);
    assert!(b.diagnostics.is_empty(), "{}", diagnostics(&b));
    assert_eq!(b.module_count, 1);
    // Left untouched: it only runs outside Roblox.
    assert!(b.code.contains(r#"require "../test/mock".Enum"#));
    assert!(b.code.contains("require(script.Missing)"));
}

#[test]
fn regenerated_sourcemap_matches_disk() {
    // Copy the stale fixture and add an entry whose file doesn't exist.
    let dir = std::env::temp_dir().join(format!("klsm-sourcemap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ReplicatedStorage")).unwrap();
    for f in ["Old.luau", "New.luau"] {
        std::fs::copy(
            Path::new("tests/fixtures/stale/ReplicatedStorage").join(f),
            dir.join("ReplicatedStorage").join(f),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("sourcemap.json"),
        r#"{ "name": "Game", "className": "DataModel", "filePaths": [], "children": [
          { "name": "ReplicatedStorage", "className": "ReplicatedStorage", "filePaths": [], "children": [
            { "name": "Old", "className": "ModuleScript", "filePaths": ["ReplicatedStorage/Old.luau"], "children": [] },
            { "name": "Gone", "className": "ModuleScript", "filePaths": ["ReplicatedStorage/Gone.luau"], "children": [] },
            { "name": "Empty", "className": "Folder", "filePaths": [], "children": [] }
          ] },
          { "name": "StarterPlayer", "className": "StarterPlayer", "filePaths": [], "children": [
            { "name": "StarterPlayerScripts", "className": "StarterPlayerScripts", "filePaths": [], "children": [] }
          ] }
        ] }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("StarterPlayerScripts")).unwrap();
    std::fs::write(
        dir.join("StarterPlayerScripts/Main.local.luau"),
        "print(1)\n",
    )
    .unwrap();

    let stale = Tree::load(&dir, None).unwrap();
    assert!(stale.is_stale());
    assert_eq!(
        stale.missing_files,
        vec!["ReplicatedStorage/Gone.luau".to_string()]
    );
    assert!(stale.write_sourcemap(&dir.join("sourcemap.json")).unwrap());

    let fresh = Tree::load(&dir, None).unwrap();
    assert!(
        !fresh.is_stale(),
        "{:?} {:?}",
        fresh.stale_files,
        fresh.missing_files
    );
    assert!(!fresh.write_sourcemap(&dir.join("sourcemap.json")).unwrap());
    let rs = fresh.child(ROOT, "ReplicatedStorage").unwrap();
    assert!(fresh.child(rs, "New").is_some());
    assert!(fresh.child(rs, "Gone").is_none());
    assert!(
        fresh.child(rs, "Empty").is_some(),
        "instances without files are kept"
    );
    // StarterPlayerScripts is nested once, not duplicated by the disk merge.
    let sp = fresh.child(ROOT, "StarterPlayer").unwrap();
    assert_eq!(fresh.node(sp).children.len(), 1);
    let sps = fresh.child(sp, "StarterPlayerScripts").unwrap();
    assert!(fresh.child(sps, "Main").is_some());
    let json = fresh.sourcemap_json();
    assert!(json.starts_with("{\n  \"name\": \"Game\""));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn external_modules_stay_in_the_game() {
    let tree = Tree::load(Path::new("tests/fixtures/basic"), None).unwrap();
    let entry = tree
        .find_file(Path::new("ReplicatedStorage/Shared/Suite.luau"))
        .unwrap();
    let external = [
        "ReplicatedStorage/Pkg".to_string(),
        "game.ReplicatedStorage.Shared.Lazy".to_string(),
    ];
    let b = bundle::bundle(&Options {
        tree: &tree,
        aliases: &Aliases::new(),
        entry,
        minify: Minify::None,
        script_name: "Bundle".into(),
        external: &external,
        internal: &[],
        include: &[],
        exclude: &[],
        prologue: "",
    });
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    // Pkg (with its lib), Lazy.A and Lazy.B are no longer bundled (10 -> 6).
    assert_eq!(b.module_count, 6);
    assert_eq!(b.external_count, 3);
    assert!(
        b.code
            .contains("require(game:GetService(\"ReplicatedStorage\"):WaitForChild(\"Pkg\"))")
    );
    assert!(b.code.contains(
        "require(game:GetService(\"ReplicatedStorage\"):WaitForChild(\"Shared\"):WaitForChild(\"Lazy\"):WaitForChild(\"A\"))"
    ));
    assert!(
        !b.inputs
            .iter()
            .any(|f| f.contains("Pkg") || f.contains("Lazy"))
    );
    assert!(parses(&b.code));
}

fn build_addons(include: &[String], minify: Minify) -> Bundle {
    let tree = Tree::load(Path::new("tests/fixtures/addons"), None).unwrap();
    let entry = tree
        .find_file(Path::new("ServerScriptService/Server/Main.server.luau"))
        .unwrap();
    bundle::bundle(&Options {
        tree: &tree,
        aliases: &Aliases::new(),
        entry,
        minify,
        script_name: "Bundle".into(),
        external: &["ReplicatedStorage".to_string()],
        internal: &["ReplicatedStorage.AddonLoader".to_string()],
        include,
        exclude: &[],
        prologue: "",
    })
}

#[test]
fn included_modules_are_bundled_for_runtime_loaders() {
    // Without `include` only what Main reaches statically: Addons + AddonLoader (internal).
    let b = build_addons(&[], Minify::None);
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    assert_eq!(b.module_count, 2);
    // Nothing bundled under Features: the stand-in would find nothing there.
    assert!(diagnostics(&b).contains("Addons.luau:5: `script` used at runtime"));
    assert_eq!(b.external_count, 0);

    let b = build_addons(
        &["ServerScriptService/Addon/Features/*/*Service.luau".to_string()],
        Minify::None,
    );
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    // + Alpha/Beta/Broken services, AlphaHelper and Beta's commands (required in a loop over
    // a table); not the story nor Gamma's helper.
    assert_eq!(b.module_count, 8);
    // `script.Parent:FindFirstChild("Features")` reaches bundled modules and Beta's loop
    // require resolves, so only Alpha's stand-in API calls (IsA, GetChildren, ...) warn.
    let d = diagnostics(&b);
    assert_eq!(d.lines().count(), 1, "{d}");
    assert!(
        d.contains("AlphaService.luau:4: `script` used at runtime"),
        "{d}"
    );
    assert!(b.inputs.iter().any(|f| f.ends_with("AlphaHelper.luau")));
    assert!(
        !b.inputs
            .iter()
            .any(|f| f.contains("story") || f.contains("Gamma"))
    );
    assert!(parses(&b.code));
}

#[test]
fn exclude_removes_included_modules() {
    let tree = Tree::load(Path::new("tests/fixtures/addons"), None).unwrap();
    let entry = tree
        .find_file(Path::new("ServerScriptService/Server/Main.server.luau"))
        .unwrap();
    let b = bundle::bundle(&Options {
        tree: &tree,
        aliases: &Aliases::new(),
        entry,
        minify: Minify::None,
        script_name: "Bundle".into(),
        external: &["ReplicatedStorage".to_string()],
        internal: &["ReplicatedStorage.AddonLoader".to_string()],
        include: &["ServerScriptService/Addon/Features/*/*Service.luau".to_string()],
        exclude: &[
            "ServerScriptService/Addon/Features/Alpha/*".to_string(),
            "ServerScriptService/Addon/Features/Typo/*".to_string(),
        ],
        prologue: "",
    });
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    // Alpha's service (and so its helper) is gone: 8 -> 6.
    assert_eq!(b.module_count, 6);
    assert!(!b.inputs.iter().any(|f| f.contains("Alpha")));
    assert!(
        diagnostics(&b)
            .contains("exclude \"ServerScriptService/Addon/Features/Typo/*\" matches no script")
    );
}

#[test]
fn include_reports_globs_that_match_nothing() {
    let b = build_addons(&["Nowhere/*.luau".to_string()], Minify::None);
    assert!(diagnostics(&b).contains("include \"Nowhere/*.luau\" matches no script"));
    let b = build_addons(
        &["ServerScriptService/Server/*.luau".to_string()],
        Minify::None,
    );
    assert!(b.has_errors(), "a Script can't be included");
}

/// `LUAU_BIN=/path/to/luau cargo test addon_loader_runs_under_luau`
#[test]
fn addon_loader_runs_under_luau() {
    let Ok(luau) = std::env::var("LUAU_BIN") else {
        return;
    };
    let include = [
        "ServerScriptService/Addon/Features/*/*Service.luau".to_string(),
        "ServerScriptService/Addon/Features/*/*Boot.luau".to_string(),
    ];
    for level in [Minify::None, Minify::Light, Minify::Full, Minify::Max] {
        let b = build_addons(&include, level);
        assert!(!b.has_errors(), "{}", diagnostics(&b));
        let script = format!(
            "game = {{ GetService = function() return {{}} end }}\nwarn = function(m) print(\"warn:\", m) end\n{}",
            b.code
        );
        let dir =
            std::env::temp_dir().join(format!("klsm-addons-{}-{level:?}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("addons.luau");
        std::fs::write(&file, script).unwrap();
        let out = std::process::Command::new(&luau)
            .arg(&file)
            .output()
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{level:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            stdout,
            "warn:\tskipped BrokenService\nAlpha,Beta\t42\ttrue\ttrue\ttrue\t3\that+move\tServer\tAlphaBoot\n",
            "{level:?}"
        );
    }
}

#[test]
fn excluded_module_required_statically_warns() {
    let tree = Tree::load(Path::new("tests/fixtures/addons"), None).unwrap();
    let entry = tree
        .find_file(Path::new("ServerScriptService/Server/Main.server.luau"))
        .unwrap();
    let b = bundle::bundle(&Options {
        tree: &tree,
        aliases: &Aliases::new(),
        entry,
        minify: Minify::None,
        script_name: "Bundle".into(),
        external: &["ReplicatedStorage".to_string()],
        internal: &["ReplicatedStorage.AddonLoader".to_string()],
        include: &["ServerScriptService/Addon/Features/*/*Service.luau".to_string()],
        exclude: &["ServerScriptService/Addon/Features/Alpha/AlphaHelper.luau".to_string()],
        prologue: "",
    });
    assert!(diagnostics(&b).contains(
        "AlphaHelper.luau: excluded by \"ServerScriptService/Addon/Features/Alpha/AlphaHelper.luau\" but still bundled: required by ServerScriptService/Addon/Features/Alpha/AlphaService.luau"
    ), "{}", diagnostics(&b));
}

#[test]
fn client_profiles_exclude_features_and_redirect_output() {
    use klsm_bundler::config::{Config, Target as BundleTarget};
    use klsm_bundler::profile::{self, Profile};
    let dir = std::env::temp_dir().join(format!("klsm-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("bundle.toml");
    let make = || Config {
        root: Path::new("tests/fixtures/addons").canonicalize().unwrap(),
        sourcemap: None,
        regenerate_sourcemap: false,
        minify: Minify::None,
        external: vec!["ReplicatedStorage".into()],
        internal: vec!["ReplicatedStorage.AddonLoader".into()],
        features: Some("*/Addon/Features".into()),
        assets: None,
        place: None,
        bundles: vec![BundleTarget {
            entry: "ServerScriptService/Server/Main.server.luau".into(),
            output: "/somewhere/Loader.server.luau".into(),
            rbxmx: Some("/somewhere/Server.rbxmx".into()),
            minify: None,
            name: None,
            external: vec![],
            internal: vec![],
            include: vec!["ServerScriptService/Addon/Features/*/*Service.luau".into()],
            exclude: vec![],
            prologue: None,
        }],
    };

    let names: Vec<String> = profile::features(&make())
        .unwrap()
        .into_iter()
        .map(|f| format!("{}:{}", f.name, f.sides.join("+")))
        .collect();
    assert_eq!(
        names,
        [
            "Alpha:ReplicatedStorage+ServerScriptService",
            "Beta:ReplicatedStorage+ServerScriptService",
            "Broken:ServerScriptService",
            "Gamma:ServerScriptService"
        ]
    );

    let acme = Profile {
        disabled: vec!["Beta".into(), "Alpha".into()],
    };
    profile::save(&config_path, "acme", &acme).unwrap();
    assert!(profile::save(&config_path, "../evil", &acme).is_err());
    assert_eq!(profile::list(&config_path).unwrap(), ["acme"]);
    let loaded = profile::load(&config_path, "acme").unwrap();
    assert_eq!(loaded.disabled, ["Alpha", "Beta"]);

    let mut config = make();
    profile::apply(&mut config, &config_path, "acme", &loaded).unwrap();
    let target = &config.bundles[0];
    assert_eq!(target.output, dir.join("dist/acme/Loader.server.luau"));
    // One package replaces the per-bundle model; the server bundle carries its installer.
    assert_eq!(target.rbxmx, None);
    assert!(target.prologue.as_deref().unwrap().contains("KlsmExact"));

    let tree = Tree::load(&config.root, None).unwrap();
    let entry = tree.find_file(&target.entry).unwrap();
    let b = bundle::bundle(&Options {
        tree: &tree,
        aliases: &Aliases::new(),
        entry,
        minify: Minify::None,
        script_name: "Bundle".into(),
        external: &config.external,
        internal: &config.internal,
        include: &target.include,
        exclude: &target.exclude,
        prologue: "",
    });
    assert!(!b.has_errors(), "{}", diagnostics(&b));
    // Only Broken's service is left besides Addons + AddonLoader.
    assert!(
        !b.inputs
            .iter()
            .any(|f| f.contains("/Alpha/") || f.contains("/Beta/"))
    );
    assert_eq!(b.module_count, 3);

    // The one-file package: server Loader first, then ReplicatedStorage with what this client
    // gets. Alpha and Beta are off, so neither of their Shared configs ships.
    assert!(klsm_bundler::build_all(&config, false, &mut |_| {}).unwrap());
    let package = profile::write_package(&config, &config_path, "acme").unwrap();
    assert_eq!(package, dir.join("dist/acme/acme.rbxmx"));
    let xml = std::fs::read_to_string(&package).unwrap();
    let names: Vec<&str> = xml
        .match_indices("<string name=\"Name\">")
        .map(|(i, m)| {
            let rest = &xml[i + m.len()..];
            &rest[..rest.find('<').unwrap()]
        })
        .collect();
    assert_eq!(
        names,
        [
            "KlsmPackage",
            "Loader",
            "ReplicatedStorage",
            "Addon",
            "Features",
            "AddonLoader"
        ]
    );
    assert!(xml.contains("<token name=\"RunContext\">1</token>"));
    assert!(xml.contains("KlSM package installer"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn package_ships_enabled_shared_features_only() {
    use klsm_bundler::config::{Config, Target as BundleTarget};
    use klsm_bundler::profile::{self, Profile};
    let dir = std::env::temp_dir().join(format!("klsm-package-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("bundle.toml");
    let mut config = Config {
        root: Path::new("tests/fixtures/addons").canonicalize().unwrap(),
        sourcemap: None,
        regenerate_sourcemap: false,
        minify: Minify::Full,
        external: vec!["ReplicatedStorage".into()],
        internal: vec!["ReplicatedStorage.AddonLoader".into()],
        features: Some("*/Addon/Features".into()),
        assets: None,
        place: None,
        bundles: vec![BundleTarget {
            entry: "ServerScriptService/Server/Main.server.luau".into(),
            output: "Loader.server.luau".into(),
            rbxmx: None,
            minify: None,
            name: None,
            external: vec![],
            internal: vec![],
            include: vec!["ServerScriptService/Addon/Features/*/*Service.luau".into()],
            exclude: vec![],
            prologue: None,
        }],
    };
    let solo = Profile {
        disabled: vec!["Beta".into()],
    };
    profile::apply(&mut config, &config_path, "solo", &solo).unwrap();
    assert!(klsm_bundler::build_all(&config, false, &mut |_| {}).unwrap());

    // A Studio "Save to File" of ReplicatedStorage.EffectDonation (binary, like Studio's default).
    let assets = dir.join("assets");
    std::fs::create_dir_all(assets.join("ReplicatedStorage")).unwrap();
    let effect = rbx_dom_weak::WeakDom::new(
        rbx_dom_weak::InstanceBuilder::new("DataModel").with_child(
            rbx_dom_weak::InstanceBuilder::new("Folder")
                .with_name("EffectDonation")
                .with_child(
                    rbx_dom_weak::InstanceBuilder::new("Part")
                        .with_name("Burst")
                        .with_child(
                            rbx_dom_weak::InstanceBuilder::new("ParticleEmitter")
                                .with_name("Spark"),
                        ),
                ),
        ),
    );
    let file = std::fs::File::create(assets.join("ReplicatedStorage/EffectDonation.rbxm")).unwrap();
    rbx_binary::to_writer(file, &effect, effect.root().children()).unwrap();
    config.assets = Some(assets.clone());

    let package = profile::write_package(&config, &config_path, "solo").unwrap();
    let xml = std::fs::read_to_string(&package).unwrap();
    // The asset sits in the package's one ReplicatedStorage folder, next to the shared modules.
    let dom = rbx_xml::from_str_default(&xml).unwrap();
    let path_of = |name: &str| {
        let inst = dom.descendants().find(|i| i.name == name).unwrap();
        dom.full_path_of(inst.referent(), ".")
    };
    assert_eq!(
        path_of("Spark"),
        "KlsmPackage.ReplicatedStorage.EffectDonation.Burst.Spark"
    );
    assert_eq!(xml.matches(">ReplicatedStorage<").count(), 1);

    // Files must sit in a service folder, or the installer would leave them in the package.
    std::fs::write(
        assets.join("Loose.rbxmx"),
        "<roblox version=\"4\"></roblox>",
    )
    .unwrap();
    assert!(profile::write_package(&config, &config_path, "solo").is_err());
    assert!(xml.contains("AlphaConfig"));
    assert!(
        !xml.contains("BetaConfig"),
        "a disabled feature's Shared config must not ship"
    );
    // Only Features is marked: count 1, "KlsmExact" (9 bytes), type 3 (bool), true.
    assert_eq!(xml.matches("AttributesSerialize").count(), 1);
    let features = xml.find("<string name=\"Name\">Features</string>").unwrap();
    let attr = xml.find(">AQAAAAkAAABLbHNtRXhhY3QDAQ==<").unwrap();
    assert!(attr > features && attr - features < 120);
    std::fs::remove_dir_all(&dir).ok();
}

/// Runs src/installer.luau against a mock place: `LUAU_BIN=… cargo test installer_runs_under_luau`.
#[test]
fn installer_runs_under_luau() {
    let Ok(luau) = std::env::var("LUAU_BIN") else {
        return;
    };
    let harness = std::fs::read_to_string("tests/installer_harness.luau").unwrap();
    let installer = std::fs::read_to_string("src/installer.luau").unwrap();
    let dir = std::env::temp_dir().join(format!("klsm-installer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("installer.luau");
    std::fs::write(&file, harness.replace("--@@INSTALLER@@", &installer)).unwrap();
    let out = std::process::Command::new(&luau)
        .arg(&file)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Old modules replaced, Studio-only content kept (EffectDonation, Alpha.Assets), the
    // switched-off feature (Gone) removed, the client Loader in place, the package emptied.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Addon,AddonLoader,EffectDonation,Loader\tAlpha\tAlphaConfig,Assets\tnew\tnew\tclient\tLoader\n"
    );
}

#[test]
fn place_assets_land_under_their_service_folder() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom};
    let mut place = WeakDom::new(
        InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("ReplicatedStorage")
                .with_name("ReplicatedStorage")
                .with_child(
                    InstanceBuilder::new("Folder")
                        .with_name("EffectDonation")
                        .with_child(InstanceBuilder::new("Sound").with_name("Ding")),
                ),
        ),
    );
    let mut package = WeakDom::new(
        InstanceBuilder::new("DataModel")
            .with_child(InstanceBuilder::new("Folder").with_name("KlsmPackage")),
    );
    let root = package.root().children()[0];
    let paths = vec!["ReplicatedStorage.EffectDonation".to_string()];
    klsm_bundler::place::copy_assets(&mut place, &paths, &mut package, root).unwrap();
    let ding = package.descendants().find(|i| i.name == "Ding").unwrap();
    assert_eq!(
        package.full_path_of(ding.referent(), "."),
        "KlsmPackage.ReplicatedStorage.EffectDonation.Ding"
    );

    let missing = vec!["ReplicatedStorage.Nope".to_string()];
    let err =
        klsm_bundler::place::copy_assets(&mut place, &missing, &mut package, root).unwrap_err();
    assert!(err.to_string().contains("\"Nope\""), "{err}");
    let bare = vec!["EffectDonation".to_string()];
    assert!(klsm_bundler::place::copy_assets(&mut place, &bare, &mut package, root).is_err());
}
