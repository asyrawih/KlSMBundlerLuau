//! Studio-only content (effects, models) pulled straight from the published place through
//! Open Cloud, so client packages can ship it without exporting files by hand.

use crate::output::folder;
use anyhow::{Context, Result, anyhow, bail};
use rbx_dom_weak::WeakDom;
use rbx_types::Ref;
use serde::Deserialize;

pub const API_KEY_ENV: &str = "ROBLOX_API_KEY";
/// Next to `bundle.toml`; for the desktop app, which doesn't see shell variables.
pub const API_KEY_FILE: &str = ".roblox-api-key";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    /// The place to read (its last saved/published version).
    pub id: u64,
    /// Instance paths copied into the package, e.g. `ReplicatedStorage.EffectDonation`.
    pub assets: Vec<String>,
}

impl Place {
    /// The log line for a package that shipped these assets; `None` when there are none.
    pub fn summary(&self) -> Option<String> {
        (!self.assets.is_empty()).then(|| {
            format!(
                "✓ {} asset(s) from place {}: {}",
                self.assets.len(),
                self.id,
                self.assets.join(", ")
            )
        })
    }
}

#[derive(Deserialize)]
struct Location {
    location: String,
}

fn get(url: &str, api_key: Option<&str>) -> Result<ureq::http::Response<ureq::Body>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent.get(url);
    if let Some(key) = api_key {
        request = request.header("x-api-key", key);
    }
    let mut response = request.call().with_context(|| format!("GET {url}"))?;
    if !response.status().is_success() {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        bail!("GET {url}: HTTP {}: {body}", response.status());
    }
    Ok(response)
}

/// The Open Cloud API key: `ROBLOX_API_KEY`, else the `.roblox-api-key` file in `dir`.
pub fn api_key(dir: &std::path::Path) -> Result<String> {
    if let Ok(key) = std::env::var(API_KEY_ENV) {
        return Ok(key.trim().to_string());
    }
    std::fs::read_to_string(dir.join(API_KEY_FILE))
        .map(|k| k.trim().to_string())
        .map_err(|_| {
            anyhow!(
                "[place] needs an Open Cloud API key: set {API_KEY_ENV} or put it in {}",
                dir.join(API_KEY_FILE).display()
            )
        })
}

/// Downloads the place file (its last saved/published version).
pub fn download(id: u64, key: &str) -> Result<WeakDom> {
    let url = format!("https://apis.roblox.com/asset-delivery-api/v1/assetId/{id}");
    let location: Location = get(&url, Some(key))?.body_mut().read_json()?;
    // The signed CDN link needs no key; ureq undoes its gzip transfer encoding.
    let bytes = get(&location.location, None)?
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()?;
    parse(&bytes).with_context(|| format!("reading place {id}"))
}

/// A place file in either format (Roblox serves `.rbxl`; `.rbxlx` starts with `<roblox `).
fn parse(bytes: &[u8]) -> Result<WeakDom> {
    if bytes.starts_with(b"<roblox!") {
        Ok(rbx_binary::from_reader(bytes)?)
    } else {
        Ok(rbx_xml::from_reader_default(bytes)?)
    }
}

/// Moves each `paths` instance from `place` into `dom` under `parent`, inside folders named
/// after its ancestors (`ReplicatedStorage.EffectDonation` → `<parent>/ReplicatedStorage/`),
/// which the package installer merges into the matching service.
pub fn copy_assets(
    place: &mut WeakDom,
    paths: &[String],
    dom: &mut WeakDom,
    parent: Ref,
) -> Result<()> {
    for path in paths {
        let segments: Vec<&str> = path
            .split('.')
            .filter(|s| !s.is_empty() && *s != "game")
            .collect();
        if segments.len() < 2 {
            bail!(
                "[place] asset {path:?} must be inside a service (e.g. ReplicatedStorage.{path})"
            );
        }
        let mut id = place.root_ref();
        for segment in &segments {
            id = place
                .get_by_ref(id)
                .unwrap()
                .children()
                .iter()
                .copied()
                .find(|&c| place.get_by_ref(c).unwrap().name == *segment)
                .ok_or_else(|| anyhow!("[place] asset {path}: no {segment:?} in the place"))?;
        }
        let mut into = parent;
        for segment in &segments[..segments.len() - 1] {
            into = folder(dom, into, segment);
        }
        place.transfer(id, dom, into);
    }
    Ok(())
}
