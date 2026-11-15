//! archive.org source: find ROM set items, index their file lists, build download URLs.

use crate::library::has_ext;
use crate::systems;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ItemHit {
    pub identifier: String,
    pub title: String,
    pub size: u64,
    pub downloads: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RomFile {
    pub name: String,
    pub size: u64,
    /// Set for members of a big archive (archive.org serves them individually).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// For torrent-backed indexes (Minerva): this file's index inside the torrent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_index: Option<u32>,
}

impl RomFile {
    pub fn url(&self, item: &str) -> String {
        self.url.clone().unwrap_or_else(|| download_url(item, &self.name))
    }
}

/// Many sets on archive.org are one multi-GB zip (or a handful of them). Those get
/// expanded into their members so single games can be picked. Sets with many files
/// are one-game-per-file (often a big zip per disc game) and are left alone.
const EXPAND_ARCHIVES_OVER: u64 = 50 * 1024 * 1024;
const EXPAND_ONLY_IF_FILES_AT_MOST: usize = 30;
/// An archive with this few members is a single game, not a container.
const MIN_MEMBERS_TO_EXPAND: usize = 4;

/// Parse archive.org's HTML listing of an archive's contents (`/download/ITEM/FILE.zip/`).
fn parse_archive_listing(html: &str, item: &str, archive: &str) -> Vec<RomFile> {
    let prefix = format!("//archive.org/download/{item}/");
    let mut out = Vec::new();
    for row in html.split("<tr>").skip(1) {
        let Some(rest) = row.split_once("<a href=\"").map(|(_, r)| r) else { continue };
        let Some((href, rest)) = rest.split_once('"') else { continue };
        let Some(enc_path) = href.strip_prefix(&prefix) else { continue };
        // enc_path = "<archive>/<url-encoded member path>"
        let member_enc = enc_path.split_once('/').map(|(_, m)| m).unwrap_or("");
        let Ok(member) = urlencoding::decode(member_enc) else { continue };
        if member.is_empty() || member.ends_with('/') {
            continue;
        }
        let size = rest.split_once("id=\"size\">").and_then(|(_, s)| s.split('<').next()).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        out.push(RomFile { name: format!("{archive}/{member}"), size, url: Some(format!("https:{href}")), torrent_index: None });
    }
    out
}

async fn list_archive(http: &reqwest::Client, item: &str, archive: &str) -> Result<Vec<RomFile>> {
    let html = http.get(format!("{}/", download_url(item, archive))).send().await?.error_for_status()?.text().await?;
    Ok(parse_archive_listing(&html, item, archive))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemIndex {
    /// archive.org "access-restricted-item": downloads need a logged-in account,
    /// so neither TorBox nor anonymous direct downloads will work.
    pub restricted: bool,
    pub files: Vec<RomFile>,
    /// Detected system for the whole set, if one stands out.
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub title: String,
    /// Unix time this index was fetched (for refreshing stale caches).
    #[serde(default)]
    pub fetched: u64,
    /// Torrent info-hash when the whole set is one torrent (Minerva).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btih: Option<String>,
}

impl ItemIndex {
    pub fn age(&self) -> std::time::Duration {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        std::time::Duration::from_secs(now.saturating_sub(self.fetched))
    }
}

fn text_list(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ; "),
        _ => String::new(),
    }
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Detect a set's system: title → identifier → file-extension vote → subject tags →
/// peek inside a few of its zips → description. Each `subject` tag is tried on its own
/// so a tag list like "nintendo; snes; rom" isn't confused by unrelated words.
async fn detect_system(http: &reqwest::Client, item: &str, meta: &Value, files: &[RomFile], deep: bool) -> Option<&'static str> {
    let md = &meta["metadata"];
    if systems::is_unsupported_platform(&text_list(md.get("title"))) {
        return None;
    }
    if let Some(s) = systems::guess_from_title(&text_list(md.get("title"))) {
        return Some(s);
    }
    if let Some(s) = systems::guess_from_title(item) {
        return Some(s);
    }
    if let Some(s) = systems::vote(files.iter().map(|f| f.name.as_str())) {
        return Some(s);
    }
    let subjects = text_list(md.get("subject"));
    if let Some(s) = subjects.split([';', ',']).find_map(|t| systems::guess_from_title(t.trim())) {
        return Some(s);
    }
    // Sets of single-ROM zips: archive.org can list a zip's contents, so look inside a few.
    let samples: Vec<&RomFile> =
        files.iter().filter(|f| deep && f.url.is_none() && (has_ext(&f.name, "zip") || has_ext(&f.name, "7z"))).take(3).collect();
    let mut inner = Vec::new();
    for f in samples {
        if let Ok(members) = list_archive(http, item, &f.name).await {
            inner.extend(members.into_iter().map(|m| m.name));
        }
    }
    if let Some(s) = systems::vote(inner.iter().map(String::as_str)) {
        return Some(s);
    }
    let desc: String = strip_tags(&text_list(md.get("description"))).chars().take(400).collect();
    systems::guess_from_title(&desc)
}

fn num(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Search archive.org items by title (software/data only). Anything containing ':' is
/// passed through as a raw archive.org query. `by_popularity` sorts by downloads,
/// otherwise archive.org's relevance order is kept.
pub async fn search_items(http: &reqwest::Client, query: &str, rows: usize, by_popularity: bool) -> Result<Vec<ItemHit>> {
    let q = if query.contains(':') {
        query.to_string()
    } else {
        let clean: String = query.chars().filter(|c| !"()\"".contains(*c)).collect();
        format!("title:({clean}) AND (mediatype:software OR mediatype:data)")
    };
    let rows = rows.to_string();
    let mut params = vec![
        ("q", q.as_str()),
        ("fl[]", "identifier"),
        ("fl[]", "title"),
        ("fl[]", "item_size"),
        ("fl[]", "downloads"),
        ("rows", rows.as_str()),
        ("output", "json"),
    ];
    if by_popularity {
        params.push(("sort[]", "downloads desc"));
    }
    let resp: Value = http.get("https://archive.org/advancedsearch.php").query(&params).send().await?.error_for_status()?.json().await?;
    let docs = resp["response"]["docs"].as_array().cloned().unwrap_or_default();
    Ok(docs
        .into_iter()
        .filter_map(|d| {
            Some(ItemHit {
                identifier: d.get("identifier")?.as_str()?.to_string(),
                title: match d.get("title") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(a)) => a.first().and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    _ => String::new(),
                },
                size: num(d.get("item_size")),
                downloads: num(d.get("downloads")),
            })
        })
        .collect())
}

/// Not ROMs: metadata, images, docs, media, and apps for other platforms.
const JUNK_EXT: &[&str] = &[
    "xml", "sqlite", "torrent", "txt", "nfo", "jpg", "jpeg", "png", "gif", "webp", "bmp", "json", "dat", "md5", "sfv", "sha1", "html",
    "htm", "pdf", "epub", "cbr", "cbz", "doc", "docx", "mp3", "ogg", "flac", "wav", "mp4", "mkv", "avi", "webm", "mov", "ipa", "apk",
    "xapk", "exe", "msi", "dmg", "deb", "appimage", "url", "lnk", "ini", "log", "srt", "ass", "jar", "swf", "jfif", "yaml", "yml", "acb",
    "awb", "hca", "psd", "svg", "ico",
];

fn is_rom_file(f: &Value) -> bool {
    if f.get("source").and_then(|s| s.as_str()) != Some("original") {
        return false;
    }
    let name = f.get("name").and_then(|s| s.as_str()).unwrap_or("");
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    !name.is_empty() && !JUNK_EXT.contains(&ext.as_str()) && !name.starts_with("__ia_thumb")
}

/// Full index of a set: big container archives are expanded into their members.
pub async fn fetch_index(http: &reqwest::Client, item: &str) -> Result<ItemIndex> {
    fetch_index_opts(http, item, true).await
}

/// `deep = false`: top-level files only, no extra requests (for live search).
async fn fetch_index_opts(http: &reqwest::Client, item: &str, deep: bool) -> Result<ItemIndex> {
    let meta: Value = http.get(format!("https://archive.org/metadata/{item}")).send().await?.error_for_status()?.json().await?;
    let files = meta["files"].as_array().with_context(|| format!("archive.org item '{item}' has no files"))?;
    let restricted = matches!(
        meta["metadata"].get("access-restricted-item"),
        Some(Value::String(s)) if s == "true"
    ) || meta["metadata"].get("access-restricted-item") == Some(&Value::Bool(true));
    let roms: Vec<RomFile> = files
        .iter()
        .filter(|f| is_rom_file(f))
        .map(|f| RomFile {
            name: f["name"].as_str().unwrap_or_default().to_string(),
            size: num(f.get("size")),
            url: None,
            torrent_index: None,
        })
        .collect();
    let may_expand = deep && roms.len() <= EXPAND_ONLY_IF_FILES_AT_MOST;
    let mut out: Vec<RomFile> = Vec::new();
    for file in roms {
        let ext = file.name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if may_expand
            && file.size >= EXPAND_ARCHIVES_OVER
            && matches!(ext.as_str(), "zip" | "7z" | "rar")
            && let Ok(members) = list_archive(http, item, &file.name).await
        {
            let members: Vec<RomFile> = members.into_iter().filter(|m| !is_junk_name(&m.name)).collect();
            if members.len() >= MIN_MEMBERS_TO_EXPAND {
                out.extend(members);
                continue;
            }
        }
        out.push(file);
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    let system = detect_system(http, item, &meta, &out, deep).await.map(str::to_string);
    let fetched = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok(ItemIndex { restricted, files: out, system, title: text_list(meta["metadata"].get("title")), fetched, btih: None })
}

fn is_junk_name(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    JUNK_EXT.contains(&ext.as_str())
}

/// Items with more files than this are big sets; live search skips them
/// (the built-in sets cover those, and their file lists are slow to fetch).
const LIVE_MAX_FILES: usize = 300;
/// How many matching items live search looks inside.
const LIVE_MAX_ITEMS: usize = 25;

/// Live game search: find archive.org items whose title matches, then list their ROM files.
/// Good for single-game uploads that aren't part of any big set. archive.org's relevance
/// order is noisy (phone/PC/modern-console uploads), so items whose title names a
/// supported system go first and unsupported platforms are dropped.
pub async fn search_games(http: &reqwest::Client, query: &str) -> Result<Vec<(ItemHit, ItemIndex)>> {
    use futures_util::StreamExt;
    let hits = search_items(http, query, 100, false).await?;
    let (mut known, unknown): (Vec<ItemHit>, Vec<ItemHit>) = hits
        .into_iter()
        .filter(|h| !systems::is_unsupported_platform(&h.title) && !systems::is_unsupported_platform(&h.identifier))
        .partition(|h| systems::guess_from_title(&h.title).is_some());
    known.extend(unknown);
    known.truncate(LIVE_MAX_ITEMS);
    let results: Vec<(ItemHit, ItemIndex)> = futures_util::stream::iter(known)
        .map(|h| async move {
            let idx = fetch_index_opts(http, &h.identifier, false).await.ok()?;
            let usable = !idx.restricted && !idx.files.is_empty() && idx.files.len() <= LIVE_MAX_FILES;
            // Keep it only if we can tell what it's for.
            let identifiable = idx.system.is_some() || idx.files.iter().any(|f| systems::guess_from_filename(&f.name).is_some());
            (usable && identifiable).then_some((h, idx))
        })
        .buffered(8)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    Ok(results)
}

pub fn download_url(item: &str, file: &str) -> String {
    let path: Vec<String> = file.split('/').map(|seg| urlencoding::encode(seg).into_owned()).collect();
    format!("https://archive.org/download/{item}/{}", path.join("/"))
}

fn cache_path(cache_dir: &Path, item: &str) -> PathBuf {
    let safe: String = item.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    cache_dir.join("archive").join(format!("{safe}.json"))
}

pub fn load_cached(cache_dir: &Path, item: &str) -> Option<ItemIndex> {
    let text = std::fs::read_to_string(cache_path(cache_dir, item)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_cached(cache_dir: &Path, item: &str, idx: &ItemIndex) -> Result<()> {
    let p = cache_path(cache_dir, item);
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(p, serde_json::to_vec(idx)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encodes_segments() {
        assert_eq!(
            download_url("nointro.snes", "Sub Dir/Super Mario World (USA).zip"),
            "https://archive.org/download/nointro.snes/Sub%20Dir/Super%20Mario%20World%20%28USA%29.zip"
        );
    }

    /// Hits the real archive.org API: `cargo test -- --ignored`
    #[tokio::test]
    #[ignore = "network"]
    async fn live_search_and_index() {
        let http = reqwest::Client::builder().user_agent("romgetter-test").build().unwrap();
        let hits = search_items(&http, "no-intro master system", 20, true).await.unwrap();
        assert!(!hits.is_empty());

        // Live game search finds single-game uploads, without junk like iOS .ipa files.
        let games = search_games(&http, "super monkey ball").await.unwrap();
        let all: Vec<&str> = games.iter().flat_map(|(_, i)| i.files.iter().map(|f| f.name.as_str())).collect();
        assert!(all.iter().any(|n| has_ext(n, "iso")), "{all:?}");
        assert!(!all.iter().any(|n| has_ext(n, "ipa")), "{all:?}");
        let idx = fetch_index(&http, "nointro.ms-mkiii").await.unwrap();
        assert!(idx.files.len() > 100, "got {}", idx.files.len());
        assert_eq!(idx.system.as_deref(), Some("mastersystem"));
        let f = idx.files.iter().find(|f| f.size < 100_000).unwrap();
        let resp = http.get(download_url("nointro.ms-mkiii", &f.name)).send().await.unwrap();
        assert!(resp.status().is_success(), "{}", resp.status());
        assert_eq!(resp.bytes().await.unwrap().len() as u64, f.size);

        // A set that is one big zip gets expanded, and members download individually.
        let idx = fetch_index(&http, "SNES-No-Intro-2024-Myrient").await.unwrap();
        assert!(idx.files.len() > 1000, "got {}", idx.files.len());
        assert_eq!(idx.system.as_deref(), Some("snes"));
        let f = idx.files.iter().find(|f| f.name.contains("Super Mario World (USA)")).unwrap();
        assert_eq!(systems::guess_for_file(&f.name), Some("snes"));
        let resp = http.get(f.url("SNES-No-Intro-2024-Myrient")).send().await.unwrap();
        assert!(resp.status().is_success(), "{}", resp.status());
        assert_eq!(resp.bytes().await.unwrap().len() as u64, f.size);
    }

    /// Detection coverage report over real search results:
    /// `cargo test detect_report -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "network; a report, run with --nocapture"]
    async fn detect_report() {
        let http = reqwest::Client::builder().user_agent("romgetter-test").build().unwrap();
        let (mut by_title, mut by_index, mut none) = (0, 0, 0);
        for q in ["no-intro", "redump", "rom set", "roms collection", "myrient"] {
            for h in search_items(&http, q, 25, true).await.unwrap() {
                let quick = systems::guess_from_title(&h.title).or_else(|| systems::guess_from_title(&h.identifier));
                let full = match quick {
                    Some(s) => {
                        by_title += 1;
                        Some(s.to_string())
                    }
                    None => {
                        if let Ok(idx) = fetch_index(&http, &h.identifier).await {
                            if idx.system.is_some() {
                                by_index += 1;
                            } else {
                                none += 1;
                            }
                            idx.system
                        } else {
                            none += 1;
                            None
                        }
                    }
                };
                println!("{:<14} {:<45} {}", full.as_deref().unwrap_or("??"), h.identifier, h.title);
            }
        }
        println!("\ntitle/id: {by_title}  via index: {by_index}  unknown: {none}");
    }

    #[test]
    fn parses_listing() {
        let html = r#"<table><tr><th>file<th>as jpg</tr>
<tr><td>SNES/<td><td>2024-03-17<td id="size"></tr>
<tr><td><a href="//archive.org/download/X/SNES.zip/SNES%2FSuper%20Mario%20World%20%28USA%29.zip">SNES/Super Mario World (USA).zip</a><td><td>2024<td id="size">840120</tr></table>"#;
        let files = parse_archive_listing(html, "X", "SNES.zip");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "SNES.zip/SNES/Super Mario World (USA).zip");
        assert_eq!(files[0].size, 840120);
        assert_eq!(files[0].url.as_deref(), Some("https://archive.org/download/X/SNES.zip/SNES%2FSuper%20Mario%20World%20%28USA%29.zip"));
    }

    #[test]
    fn filters_junk() {
        assert!(is_rom_file(&serde_json::json!({"name": "a.7z", "source": "original"})));
        assert!(!is_rom_file(&serde_json::json!({"name": "a_files.xml", "source": "original"})));
        assert!(!is_rom_file(&serde_json::json!({"name": "a.7z", "source": "derivative"})));
    }
}
