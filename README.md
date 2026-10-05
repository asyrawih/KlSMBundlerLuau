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
minify = "none"                 # none | light | full | max
regenerate_sourcemap = true     # default: rewrite sourcemap.json whenever it lags behind the folder
external = ["ReplicatedStorage"] # modules here stay in the game instead of being bundled

[[bundle]]
entry  = "ServerScriptService/Server/Main.server.luau"
output = "dist/Server.server.luau"
rbxmx  = "dist/Server.rbxmx"    # optional: drag-into-Studio model

[[bundle]]
entry  = "StarterPlayerScripts/Client/Main.local.luau"
output = "dist/Client.local.luau"
external = ["ReplicatedStorage.Packages"]   # per-bundle additions to the list above
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
| Loops over a table of paths | `for _, m in { Commands.A, Commands.B } do require(m) end` |
| String requires | `require("./Foo")`, `require("../Foo")`, `require("@self/Foo")`, `.luaurc` aliases (`@shared/Foo`) |

The instance tree comes from `sourcemap.json`; scripts on disk that the sourcemap doesn't list yet are
merged in from the folder layout (`.luau` ModuleScript, `.server.luau` Script, `.local.luau`
LocalScript, `.client.luau` Script/Client, `init.*` = the folder's own script). StarterPlayerScripts
and StarterCharacterScripts are placed under StarterPlayer as in the real DataModel.
`klsm tree --root <folder>` prints what it sees.

Studio only rewrites `sourcemap.json` when it feels like it, so it lags behind after moving or
adding scripts. So every `build` and `watch` with a config writes the merged view back in Studio's
format whenever it's stale: scripts found on disk are added and entries whose file is gone are
dropped, so tools like Luau LSP see the same tree. `regenerate_sourcemap = false` turns that off;
`klsm sourcemap` does it on demand, and `--regenerate-sourcemap` does it for a single `--entry`
build.

## External modules

`external` (top level, per `[[bundle]]`, or `--external <path>`) lists instance paths, from `game`,
whose ModuleScripts are left in the place rather than copied into the bundle. Every require that
resolves under one of them is rewritten to an absolute path such as
`require(game:GetService("ReplicatedStorage"):WaitForChild("Shared"):WaitForChild("X"))`, because
`script`-relative paths no longer work once the requiring module lives inside the bundle. Those
modules are not walked, so whatever they require themselves is not bundled either.

## Runtime loaders (`include` / `internal`)

A loader that finds modules by scanning a folder (`for _, f in Features:GetChildren() do
require(f[f.Name .. "Service"]) end`) can't be followed statically. `include` lists globs of module
files (relative to `root`, `*` stops at `/`, `**` crosses folders) that are bundled anyway, together
with everything they require:

```toml
internal = ["ReplicatedStorage.AddonLoader"]   # bundle these even though they sit under `external`

[[bundle]]
entry   = "ServerScriptService/Server/Main.server.luau"
output  = "dist/Server.server.luau"
include = ["Addon/Server/Features/*/*Service.luau"]
```

At runtime the `script` stand-in sees the bundled modules as an instance tree: `GetChildren`,
`GetDescendants`, `FindFirstChild` (nil when nothing bundled is there), `IsA`, `ClassName`
(`ModuleScript`, or `Folder` for the folders between them), and `require` of a stand-in loads the
bundled module. The loader itself must run inside the bundle (a module left in the game calls the
real `require`, which rejects stand-ins), hence `internal` when it lives under an external path.
`exclude` (same glob form) takes files back out of what `include` matched, e.g.
`["Addon/Server/Features/Affiliate/*"]` to ship without that addon. A glob in either list that
matches nothing is a warning, which catches typos. In the entry, `script` paths and aliases
(`script.Parent.Parent.Addon:FindFirstChild("Features")`) go through a stand-in at the entry's
original place, since the bundle Script itself lives elsewhere; other uses of `script` there stay
the bundle Script. A `not statically resolvable` warning in an included module usually means
another folder for `include`. `--include <glob>` / `--internal <path>` do the same for single-entry builds.

## Client profiles

Some clients don't get every feature. With `features = "Addon/*/Features"` in `bundle.toml` (one
folder per feature inside each match; the `*` part names the side), a profile in
`clients/<name>.toml` lists the features that client doesn't get:

```toml
disabled = ["Affiliate", "MimicParty"]
```

`klsm build --client <name>` excludes those folders from every bundle and writes everything to
`dist/<name>/`, ending with **one model, `<name>.rbxmx`**, to drop into ServerScriptService:

```
KlsmPackage
├─ Loader               server bundle; starts with a small installer
└─ ReplicatedStorage    client bundle (Script, RunContext Client) + every module under
                        `external` this client gets, e.g. Addon/Features without the
                        switched-off features
```

When the server starts, the installer merges each service-named folder into that service:
scripts and modules replace same-named ones, folders merge (Studio-only content in them stays), and
a feature folder (`features` under `external`, marked `KlsmExact`) also loses features the package
doesn't have, so nothing has to be deleted from the client's place by hand. Replace the old package
when updating.

Studio-only content the code depends on (effects, models, sounds: anything Script Sync doesn't
sync) can ship in the package too, read from the place's last saved version through Open Cloud:

```toml
[place]
id = 82391043752226
assets = ["ReplicatedStorage.EffectDonation", "ReplicatedStorage.Asset"]
```

Paths can start at any service (`Workspace.Map`, `ServerStorage.Models`); the installer merges each
into its service. The desktop app edits the place ID, the key and this list (sidebar, *Roblox place*).

The Open Cloud API key comes from `ROBLOX_API_KEY`, or a `.roblox-api-key` file next to
`bundle.toml` (gitignored; the desktop app doesn't see shell variables). Save the place in Studio
before building, or the package gets the previous version. Without a key (or offline), export by hand instead: in Studio, right-click the instance → *Save to File* and save
the `.rbxm` (or `.rbxmx`) into a folder laid out by service, then point `assets` at it:

```toml
assets = "assets"     # assets/ReplicatedStorage/EffectDonation.rbxm → ReplicatedStorage.EffectDonation
```

Each file's contents land under the folders its path names, so the installer merges them into
the place like everything else. Re-export after changing them in Studio. A module that's excluded but still required statically by something bundled is
bundled anyway, with a warning naming who requires it.

### Desktop app

`app/` is a Tauri front-end for the same thing: open a `bundle.toml`, add clients, switch features
on or off (saved to `clients/<name>.toml` as you click), build, and show the package in Finder.

```sh
cd app && cargo tauri dev      # run
cd app && cargo tauri build    # KlSM Bundler.app + .dmg in app/target/release/bundle
```

## Runtime behaviour

- Each module runs once and is cached, like Roblox `require`.
- Loading a module that is already loading raises `Requested module was required recursively`,
  as in Roblox. A cycle is only warned about when every require in it runs at load time (top
  level, including top-level `if`/`do`/loops and `(function() … end)()`); one require inside a
  function body or callback makes it lazy and fine. The warning names each require's line:
  `circular eager require A (A.luau:12) -> B (B.luau:5) -> A`. A local function called at the
  top level still counts as lazy.
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
  a stand-in that knows `Name`, `Parent`, `ClassName`, `GetFullName()`, child paths and the bundled
  modules around it (see above); other Instance APIs (`GetAttribute`, …) are not available. In the
  entry, `script` is the bundle script.

## Minify and source maps

- `light` strips comments and indentation but keeps every newline, so line numbers stay exact.
- `full` also joins lines: after the `--!` directives each module is a single line (that one
  newline per module is what lets traces still name the module). Only `none` keeps the
  `-- Bundled by …` header and the per-module file comments.
- `max` is `full` plus renaming every local to a short name (darklua's `rename_variables`), about
  20% smaller again. Lines stay where `full` put them, so traces still name the module, but
  local names in Roblox error messages become `a`, `b`, ….

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

- Per-target filtering (e.g. refusing server-only modules in a client bundle).
