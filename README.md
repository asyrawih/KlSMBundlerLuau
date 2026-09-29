# KlSMBundlerLuau

Bundles a Roblox Studio **Script Sync** project into single, self-contained Luau scripts: one entry
plus every ModuleScript it reaches through `require`.

```
klsm build                      # every [[bundle]] in ./bundle.toml
klsm watch                      # rebuild on every save
klsm trace dist/Server.server.luau.map.json < error.log   # map bundle lines back to files
klsm sourcemap                  # rewrite sourcemap.json from the folder layout
```

## Setup

```sh
cargo install --path .          # installs `klsm`
```

`bundle.toml` (paths are relative to this file):

```toml
root = "../RessoMusic"          # the Script Sync folder (maps to `game`)
minify = "none"                 # none | light | full
regenerate_sourcemap = true     # rewrite sourcemap.json whenever it lags behind the folder

[[bundle]]
entry  = "ServerScriptService/Server/Main.server.luau"
output = "dist/Server.server.luau"
rbxmx  = "dist/Server.rbxmx"    # optional: drag-into-Studio model

[[bundle]]
entry  = "StarterPlayerScripts/Client/Main.local.luau"
output = "dist/Client.local.luau"
```

Single entry without a config: `klsm build --root ../RessoMusic --entry <file> -o out.luau`.

Output can also go straight into the sync folder (e.g.
`../RessoMusic/ServerScriptService/Bundle.server.luau`) so Studio picks it up; watch mode ignores its
own outputs. Remember to disable the original entry script if you do that, or both will run.

## What it resolves

| Source | Example |
|---|---|
| `script`-relative | `require(script.Parent.Foo)`, `require(script.lib)` |
| Services & aliases | `local RS = game:GetService("ReplicatedStorage")` … `require(RS.Shared.X)` |
| Lookups with literal names | `:WaitForChild("X")`, `:FindFirstChild("X")`, `["X"]`, `:FindFirstAncestor("X")` |
| String requires | `require("./Foo")`, `require("../Foo")`, `require("@self/Foo")`, `.luaurc` aliases (`@shared/Foo`) |

The instance tree comes from `sourcemap.json`; scripts on disk that the sourcemap doesn't list yet are
merged in from the folder layout (`.luau` ModuleScript, `.server.luau` Script, `.local.luau`
LocalScript, `.client.luau` Script/Client, `init.*` = the folder's own script). StarterPlayerScripts
and StarterCharacterScripts are placed under StarterPlayer as in the real DataModel.
`klsm tree --root <folder>` prints what it sees.

Studio only rewrites `sourcemap.json` when it feels like it, so it lags behind after moving or
adding scripts. `klsm sourcemap` (or `regenerate_sourcemap = true` / `--regenerate-sourcemap` on
`build` and `watch`) writes the merged view back in Studio's format: scripts found on disk are
added and entries whose file is gone are dropped, so tools like Luau LSP see the same tree.

## Runtime behaviour

- Each module runs once and is cached, like Roblox `require`.
- Loading a module that is already loading raises `Requested module was required recursively`;
  static cycles are only warned about, because lazy requires inside functions are fine.
- A module that doesn't return exactly one value raises Roblox's error message.
- Requires inside type annotations (`typeof(require(x))`) are left alone and don't pull modules in.
- `export type` is turned into `type` (exports are illegal inside the wrapper function).
- The bundle starts with `--!nocheck`; `--!native` / `--!optimize` from the entry are kept.

### Diagnostics

- **error** (bundle not written): syntax errors, requiring a Script/LocalScript, a ModuleScript with
  no synced file.
- **warning**: dynamic requires (`require(folder[name])`) and paths that don't exist in the tree —
  both are left as real `require` calls, which only fail if they run (like Roblox). `--strict`
  turns warnings into failures.
- **warning**: `script` used for anything other than a require path. Inside modules `script` becomes
  a stand-in that knows `Name`, `Parent`, `ClassName`, `GetFullName()` and child paths; Instance APIs
  (`GetAttribute`, `GetChildren`, …) are not available. In the entry, `script` is the bundle script.

## Minify and source maps

- `light` strips comments and indentation but keeps every newline, so line numbers stay exact.
- `full` also joins lines: after the `--!` directives each module is a single line (that one
  newline per module is what lets traces still name the module). Only `none` keeps the
  `-- Bundled by …` header and the per-module file comments.

Every build writes `<output>.map.json`. `klsm trace <map> 352` prints the original file and line;
without line numbers it rewrites a pasted Roblox log (`Server:352: …` and
`Script 'ServerScriptService.Server', Line 352`).

## Tests

```sh
cargo test
LUAU_BIN=/path/to/luau cargo test runs_under_luau              # executes bundles with the Luau CLI
KLSM_REAL_PROJECT=../RessoMusic cargo test --release real_project  # lossless-minify check on a real project
```

## Not done yet

- Renaming locals when minifying (only whitespace/comments are removed).
- Per-target filtering (e.g. refusing server-only modules in a client bundle).
