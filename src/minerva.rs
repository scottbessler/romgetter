//! Minerva (minerva-archive.org): the torrent-based successor to Myrient. It mirrors
//! No-Intro, Redump, etc. as directory listings; each system folder is one torrent, and
//! each file lists its index inside it (`data-m` = BitTorrent "select only" index).
//! Most of these torrents are already cached on TorBox, so adding one is instant and we
//! then download just the file we want.

use crate::archive::{ItemIndex, RomFile};
use crate::systems;
use anyhow::{Context, Result, bail};

const BASE: &str = "https://minerva-archive.org";
/// Groups whose system folders we index.
pub const GROUPS: &[&str] = &["No-Intro", "Redump"];
/// A few public trackers; TorBox mostly needs only the info-hash for cached torrents.
const TRACKERS: &str = "&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce\
&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce";

/// Cache/set key for a folder, e.g. "minerva:Redump/Sony - PlayStation 3".
pub fn set_key(group: &str, folder: &str) -> String {
    format!("minerva:{group}/{folder}")
}

pub fn parse_key(key: &str) -> Option<(&str, &str)> {
    key.strip_prefix("minerva:")?.split_once('/')
}

pub fn magnet(btih: &str) -> String {
    format!("magnet:?xt=urn:btih:{btih}&dn=Minerva_Myrient{TRACKERS}")
}

pub fn btih_of(magnet: &str) -> Option<String> {
    let rest = magnet.split("btih:").nth(1)?;
    let h: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
    (h.len() == 40 || h.len() == 32).then(|| h.to_lowercase())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Folder {
    pub name: String,
    pub size: u64,
}

/// Folders that aren't games (or are noisy duplicates of the main set).
const SKIP: &[&str] = &[
    "bios",
    "disc keys",
    "source code",
    "memory card",
    "(private)",
    "(aftermarket)",
    "update",
    "dlc",
    "theme",
    "avatar",
    "firmware",
    "kiosk",
    "headerless",
    "(program)",
    "manual",
    "(misc)",
    "audio cd",
    "video",
    // card scans, dev/system content, and encrypted variants that have decrypted twins
    "e-reader",
    "spotpass",
    "dev roms",
    "pre-install",
    "deprecated",
    "(encrypted)",
    "(cdn)",
    // PSN packages need license files to run; the disc dumps are indexed instead
    "(psn)",
    // only cue/gdi descriptors or subchannel data (a few hundred bytes), not the tracks
    "gdi files",
    "sbi subchannels",
];

/// Which system (if any) a Minerva folder name holds games for.
pub fn folder_system(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if SKIP.iter().any(|s| lower.contains(s)) {
        return None;
    }
    systems::guess_from_title(name)
}

fn parse_size(s: &str) -> u64 {
    let mut it = s.split_whitespace();
    let n: f64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let mult = match it.next().unwrap_or("B").to_ascii_uppercase().as_str() {
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0 * 1024.0,
        "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "TB" | "TIB" => 1024.0_f64.powi(4),
        _ => 1.0,
    };
    (n * mult) as u64
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">")
}

/// One `<div class="entry">` of a listing.
struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
    btih: Option<String>,
    index: Option<u32>,
}

fn parse_entries(html: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for chunk in html.split("<div class=\"entry\"").skip(1) {
        let chunk = chunk.split("</div>").next().unwrap_or(chunk);
        // first link: the name
        let Some((_, after_a)) = chunk.split_once("<a href=\"") else { continue };
        let Some((_, rest)) = after_a.split_once('>') else { continue };
        let Some((label, rest)) = rest.split_once("</a>") else { continue };
        let is_dir = label.contains("<strong>");
        let name = unescape(label.replace("<strong>", "").replace("</strong>", "").trim().trim_end_matches('/'));
        if name == ".." || name.is_empty() {
            continue;
        }
        let size = rest.split_once("<span>").and_then(|(_, s)| s.split_once("</span>")).map(|(s, _)| parse_size(s.trim())).unwrap_or(0);
        let btih = btih_of(rest);
        let index = rest.split_once("data-m=\"").and_then(|(_, s)| s.split('"').next()).and_then(|s| s.parse().ok());
        out.push(Entry { name, is_dir, size, btih, index });
    }
    out
}

fn browse_url(path: &str) -> String {
    let enc: Vec<String> = path.split('/').map(|s| urlencoding::encode(s).into_owned()).collect();
    format!("{BASE}/browse/{}/", enc.join("/"))
}

async fn get(http: &reqwest::Client, url: &str) -> Result<String> {
    // Minerva sits behind Cloudflare; a browser-ish UA avoids being served a challenge.
    let resp =
        http.get(url).header(reqwest::header::USER_AGENT, "Mozilla/5.0 (X11; Linux x86_64) romgetter").send().await?.error_for_status()?;
    Ok(resp.text().await?)
}

/// The system folders of a group (e.g. all of No-Intro).
pub async fn list_group(http: &reqwest::Client, group: &str) -> Result<Vec<Folder>> {
    let html = get(http, &browse_url(group)).await.with_context(|| format!("listing Minerva {group}"))?;
    let folders: Vec<Folder> =
        parse_entries(&html).into_iter().filter(|e| e.is_dir).map(|e| Folder { name: e.name, size: e.size }).collect();
    if folders.is_empty() {
        bail!("Minerva {group}: no folders found (page layout changed?)");
    }
    Ok(folders)
}

/// Index one system folder: its files, the folder's torrent, and each file's index in it.
pub async fn fetch_folder(http: &reqwest::Client, group: &str, folder: &str) -> Result<ItemIndex> {
    let html = get(http, &browse_url(&format!("{group}/{folder}"))).await?;
    let entries = parse_entries(&html);
    let mut btih = None;
    let mut files = Vec::new();
    for e in entries.into_iter().filter(|e| !e.is_dir) {
        if btih.is_none() {
            btih.clone_from(&e.btih);
        }
        // a folder is one torrent; skip anything that isn't part of it
        if e.btih.is_some() && e.btih != btih {
            continue;
        }
        files.push(RomFile { name: e.name, size: e.size, url: None, torrent_index: e.index, title: None, system: None });
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok(ItemIndex {
        restricted: false,
        files,
        system: folder_system(folder).map(str::to_string),
        title: format!("Minerva · {group} · {folder}"),
        fetched: now,
        btih,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"
<div class="entry" data-name="..">
    <a href="/browse/./No-Intro/" draggable="false">../</a>
</div>
<div class="entry" data-name="nintendo - super nintendo entertainment system">
    <a href="/browse/./No-Intro/Nintendo%20-%20Super%20Nintendo%20Entertainment%20System/" draggable="false"><strong>Nintendo - Super Nintendo Entertainment System/</strong></a>
    <span>3.92 GB</span>
    <a href="javascript:void(0)" onclick="downloadMagnet('magnet:?xt=urn:btih:93e0d2060d1b05f5b19d0a9e40011b51a76a933c&amp;dn=Minerva_Myrient')" data-m="279" title="Download Magnet">x</a>
</div>
<div class="entry" data-name="chrono trigger (usa).zip">
    <a href="/rom?id=2898398" draggable="false">Chrono Trigger (USA).zip</a>
    <span>2.93 MB</span>
    <a href="javascript:void(0)" onclick="downloadMagnet('magnet:?xt=urn:btih:93e0d2060d1b05f5b19d0a9e40011b51a76a933c&amp;dn=Minerva_Myrient')" data-m="535" title="Download Magnet">x</a>
</div>
<div class="entry" data-name="den&#39;z traffic panic (japan).zip">
    <a href="/rom?id=1" draggable="false">Den&#39;z Traffic Panic (Japan).zip</a>
    <span>820.43 KB</span>
    <a href="javascript:void(0)" onclick="downloadMagnet('magnet:?xt=urn:btih:93e0d2060d1b05f5b19d0a9e40011b51a76a933c&amp;dn=Minerva_Myrient')" data-m="7" title="Download Magnet">x</a>
</div>"#;

    #[test]
    fn parses_listing() {
        let e = parse_entries(LISTING);
        assert_eq!(e.len(), 3);
        assert!(e[0].is_dir);
        assert_eq!(e[0].name, "Nintendo - Super Nintendo Entertainment System");
        assert_eq!(e[1].name, "Chrono Trigger (USA).zip");
        assert_eq!(e[1].index, Some(535));
        assert_eq!(e[1].btih.as_deref(), Some("93e0d2060d1b05f5b19d0a9e40011b51a76a933c"));
        assert_eq!(e[1].size, (2.93 * 1024.0 * 1024.0) as u64);
        assert_eq!(e[2].name, "Den'z Traffic Panic (Japan).zip");
    }

    #[test]
    fn picks_game_folders() {
        assert_eq!(folder_system("Nintendo - Super Nintendo Entertainment System"), Some("snes"));
        assert_eq!(folder_system("Sony - PlayStation 3"), Some("ps3"));
        assert_eq!(folder_system("Sony - PlayStation 3 - Disc Keys TXT"), None);
        assert_eq!(folder_system("Nintendo - GameCube - BIOS Images"), None);
        assert_eq!(folder_system("Source Code - Nintendo - Game Boy Advance"), None);
        assert_eq!(folder_system("Acorn - Archimedes"), None);
        assert_eq!(folder_system("Nintendo - Game Boy Advance (e-Reader)"), None);
        assert_eq!(folder_system("Nintendo - Nintendo 3DS (Decrypted)"), Some("n3ds"));
        assert_eq!(folder_system("Nintendo - Nintendo 3DS (Encrypted)"), None);
        assert_eq!(folder_system("Sony - PlayStation 3 (PSN) (Content)"), None);
        assert_eq!(folder_system("Sega - Dreamcast - GDI Files"), None);
        assert_eq!(folder_system("Sega - Dreamcast"), Some("dreamcast"));
    }

    #[test]
    fn magnets() {
        let m = magnet("93e0d2060d1b05f5b19d0a9e40011b51a76a933c");
        assert_eq!(btih_of(&m).as_deref(), Some("93e0d2060d1b05f5b19d0a9e40011b51a76a933c"));
        assert_eq!(parse_key(&set_key("Redump", "Sony - PlayStation 3")), Some(("Redump", "Sony - PlayStation 3")));
    }

    /// `cargo test -- --ignored minerva_live`
    #[tokio::test]
    #[ignore = "network"]
    async fn minerva_live() {
        let http = reqwest::Client::new();
        let folders = list_group(&http, "No-Intro").await.unwrap();
        assert!(folders.iter().any(|f| f.name == "Nintendo - Super Nintendo Entertainment System"));
        let picked: Vec<_> = folders.iter().filter_map(|f| folder_system(&f.name).map(|s| (&f.name, s))).collect();
        assert!(picked.len() > 30, "{}", picked.len());
        let idx = fetch_folder(&http, "No-Intro", "Nintendo - Super Nintendo Entertainment System").await.unwrap();
        assert_eq!(idx.btih.as_deref(), Some("93e0d2060d1b05f5b19d0a9e40011b51a76a933c"));
        let ct = idx.files.iter().find(|f| f.name == "Chrono Trigger (USA).zip").unwrap();
        assert_eq!(ct.torrent_index, Some(535));
    }
}
