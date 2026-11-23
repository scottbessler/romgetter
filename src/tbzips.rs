//! ROM-set torrents already in the TorBox account (e.g. the "1G1R - Redump - Sony -
//! PlayStation" torrents listed on the Emulation General wiki). Each becomes a
//! searchable set. TorBox packs a torrent with 100+ files into one zip: its central
//! directory is read over HTTP range requests, and a game is later pulled out of the zip
//! by `jobs.rs` exactly like a zipped Minerva folder. Smaller torrents keep their files
//! loose, and TorBox's file list is the index.

use crate::archive::{ItemIndex, RomFile};
use crate::library::has_ext;
use crate::torbox::{Item, Kind, TorBox};
use anyhow::{Result, anyhow, bail};

/// Cache/set key prefix: "torbox:<info-hash>".
const PREFIX: &str = "torbox:";
/// Smaller zips aren't worth indexing as a set (and are usually single games).
const MIN_SIZE: f64 = 500_000_000.0;
/// A torrent of loose files needs this many games to count as a set.
const MIN_LOOSE_GAMES: usize = 3;

pub fn set_key(hash: &str) -> String {
    format!("{PREFIX}{}", hash.to_lowercase())
}

pub fn parse_key(key: &str) -> Option<&str> {
    key.strip_prefix(PREFIX)
}

/// A torrent in the account worth indexing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    /// TorBox's torrent id.
    pub id: u64,
    pub hash: String,
    pub system: String,
    pub title: String,
    /// TorBox packed it into one zip (else its files are loose).
    #[serde(default = "yes")]
    pub zipped: bool,
}

fn yes() -> bool {
    true
}

impl Candidate {
    pub fn key(&self) -> String {
        set_key(&self.hash)
    }
}

/// The folder name a torrent's files sit in; TorBox's item name is sometimes just the hash.
fn folder_name(item: &Item) -> String {
    let path = item.files.first().map(|f| f.name.as_str()).unwrap_or("");
    let folder = path.split('/').next().filter(|f| !f.is_empty() && *f != path).unwrap_or(&item.name);
    folder.replace("&amp;", "&")
}

/// Whether a torrent is a usable ROM set: a big zip, or at least a few loose games.
fn is_set(i: &Item) -> Option<bool> {
    if i.is_zipped() {
        return (i.files[0].size >= MIN_SIZE && has_ext(&i.files[0].name, "zip")).then_some(true);
    }
    (i.files.iter().filter(|f| is_game(&f.name)).count() >= MIN_LOOSE_GAMES).then_some(false)
}

/// Cached torrents whose name says which system they hold. Minerva folders are skipped:
/// they're indexed from Minerva's own listings (with torrent indices).
pub fn candidates(items: &[Item]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = items
        .iter()
        .filter(|i| (i.download_present || i.download_finished) && !i.files.is_empty())
        .filter(|i| !i.name.starts_with("Minerva") && !i.files.iter().any(|f| f.name.starts_with("Minerva_Myrient/")))
        .filter_map(|i| {
            let zipped = is_set(i)?;
            let title = folder_name(i);
            // PC-style repacks (installer chunks, not ROMs)
            if ["repack", "fitgirl", "dodi"].iter().any(|w| title.to_lowercase().contains(w)) {
                return None;
            }
            let system = crate::systems::guess_from_title(&title)?.to_string();
            Some(Candidate { id: i.id, hash: i.hash.to_lowercase(), system, title, zipped })
        })
        .collect();
    out.sort_by(|a, b| a.hash.cmp(&b.hash));
    out.dedup_by(|a, b| a.hash == b.hash);
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// Files in these sets that aren't games (metadata, extras, video riding along).
fn is_extra(name: &str) -> bool {
    const EXTS: &[&str] = &[
        "sqlite", "xml", "torrent", "pdf", "exe", "dat", "parts", "mkv", "mp4", "avi", "srt", "nzb", "log", "json", "ini", "bat", "cmd",
        "sh",
    ];
    crate::jobs::is_junk(name) || EXTS.iter().any(|e| has_ext(name, e))
}

/// A game file of a set (full path inside the torrent or zip).
fn is_game(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    !path.ends_with('/') && !is_extra(base) && !path.to_lowercase().contains("goodies/")
}

/// The games in one candidate: the zip's entries, or the torrent's loose files.
pub async fn fetch_index(tb: &TorBox, http: &reqwest::Client, c: &Candidate) -> Result<ItemIndex> {
    let item = tb.get(Kind::Torrent, c.id).await.map_err(|e| anyhow!("{} is no longer on TorBox: {e:#}", c.title))?;
    if !item.hash.eq_ignore_ascii_case(&c.hash) {
        bail!("TorBox item {} isn't {} any more", c.id, c.title);
    }
    let files = if item.is_zipped() { zip_files(tb, http, &item, c).await? } else { loose_files(&item) };
    Ok(ItemIndex {
        restricted: false,
        files,
        system: Some(c.system.clone()),
        // also TorBox's display name for the torrent (see App::minerva_name)
        title: c.title.clone(),
        fetched: crate::library::now_secs(),
        btih: Some(c.hash.clone()),
    })
}

/// A loose torrent's game files. File ids double as torrent indices unless TorBox has
/// only part of the torrent (then they're renumbered).
fn loose_files(item: &Item) -> Vec<RomFile> {
    let partial = item.is_partial();
    item.files
        .iter()
        .filter(|f| f.size > 0.0 && is_game(&f.name))
        .map(|f| RomFile {
            name: f.file_name(),
            size: f.size as u64,
            url: None,
            torrent_index: (!partial).then(|| u32::try_from(f.id).ok()).flatten(),
            title: None,
        })
        .collect()
}

async fn zip_files(tb: &TorBox, http: &reqwest::Client, item: &Item, c: &Candidate) -> Result<Vec<RomFile>> {
    let url = tb.request_dl(Kind::Torrent, item.id, item.files[0].id).await?;
    let claimed = item.files[0].size as u64;
    let (entries, served) =
        crate::remotezip::list_sized(http, &url).await.map_err(|e| anyhow!("TorBox's zip of {} can't be read: {e:#}", c.title))?;
    if served.is_some_and(|s| s < claimed) {
        bail!("TorBox's zip of {} is cut off", c.title);
    }
    Ok(entries
        .iter()
        // stored entries only: `remotezip::extract` can't inflate
        .filter(|e| e.method == 0 && e.size > 0 && is_game(&e.name))
        .map(|e| RomFile { name: e.base_name().to_string(), size: e.size, url: None, torrent_index: None, title: None })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(v: serde_json::Value) -> Item {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn picks_rom_sets() {
        let items = vec![
            item(serde_json::json!({"id": 1, "hash": "AA", "name": "cd240a9644e5808ca04bc3f503071679633b1bb9", "download_present": true,
                "files": [{"id": 0, "name": "1G1R - Redump - Nintendo - GameCube/1G1R - Redump - Nintendo - GameCube.zip", "size": 5e11, "zipped": true}]})),
            item(serde_json::json!({"id": 2, "hash": "bb", "name": "Moana.2026.2160p", "download_present": true,
                "files": [{"id": 0, "name": "Moana.2026.2160p/Moana.2026.2160p.zip", "size": 5e10, "zipped": true}]})),
            item(serde_json::json!({"id": 3, "hash": "cc", "name": "Minerva - Redump - Sega - Saturn", "download_present": true,
                "files": [{"id": 0, "name": "Minerva_Myrient/Minerva_Myrient.zip", "size": 9e11, "zipped": true}]})),
            item(serde_json::json!({"id": 4, "hash": "dd", "name": "1G1R - Redump - Sony - PSP & PSN", "download_present": true,
                "files": [{"id": 0, "name": "1G1R - Redump - Sony - PlayStation Portable &amp; PSN(miniS+DLC+Themes)/x.zip", "size": 4e11, "zipped": true}]})),
            item(serde_json::json!({"id": 5, "hash": "ee", "name": "1G1R - No-Intro - Atari - Jaguar", "download_present": true,
                "files": [{"id": 0, "name": "1G1R - No-Intro - Atari - Jaguar/a.zip", "size": 1e6}, {"id": 1, "name": "1G1R - No-Intro - Atari - Jaguar/b.zip", "size": 1e6},
                          {"id": 2, "name": "1G1R - No-Intro - Atari - Jaguar/c.j64", "size": 1e6}, {"id": 3, "name": "1G1R - No-Intro - Atari - Jaguar/00 Goodies/Emulators/x.zip", "size": 1e6},
                          {"id": 4, "name": "1G1R - No-Intro - Atari - Jaguar/readme.txt", "size": 1e3}]})),
            // too few games
            item(serde_json::json!({"id": 6, "hash": "ff", "name": "1G1R - No-Intro - Nintendo - N64", "download_present": true,
                "files": [{"id": 0, "name": "1G1R - No-Intro - Nintendo - N64/a.zip", "size": 1e6}, {"id": 1, "name": "1G1R - No-Intro - Nintendo - N64/b.zip", "size": 1e6}]})),
            item(serde_json::json!({"id": 8, "hash": "88", "name": "Zelda [FitGirl Switch 2 Repack]", "download_present": true,
                "files": [{"id": 0, "name": "The Legend of Zelda - Ocarina of Time [FitGirl Switch 2 Repack]/fg-01.bin", "size": 1e9},
                          {"id": 1, "name": "The Legend of Zelda - Ocarina of Time [FitGirl Switch 2 Repack]/fg-02.bin", "size": 1e9},
                          {"id": 2, "name": "The Legend of Zelda - Ocarina of Time [FitGirl Switch 2 Repack]/fg-03.bin", "size": 1e9}]})),
            item(serde_json::json!({"id": 7, "hash": "77", "name": "Greys Anatomy S01E01-S22E18", "download_present": true,
                "files": [{"id": 0, "name": "Greys/a.mkv", "size": 1e9}, {"id": 1, "name": "Greys/b.mkv", "size": 1e9}, {"id": 2, "name": "Greys/c.mkv", "size": 1e9}]})),
        ];
        let got = candidates(&items);
        let sys: Vec<_> = got.iter().map(|c| (c.id, c.system.as_str(), c.zipped)).collect();
        assert_eq!(sys, vec![(5, "atarijaguar", false), (1, "gc", true), (4, "psp", true)], "{got:?}");
        let names: Vec<_> = loose_files(&items[4]).into_iter().map(|f| (f.name, f.torrent_index)).collect();
        assert_eq!(names, vec![("a.zip".into(), Some(0)), ("b.zip".into(), Some(1)), ("c.j64".into(), Some(2))]);
        assert_eq!(got[1].title, "1G1R - Redump - Nintendo - GameCube");
        assert_eq!(got[1].hash, "aa");
    }

    #[tokio::test]
    #[ignore = "network + TorBox key from the config"]
    async fn index_live() {
        let paths = crate::config::Paths::new().unwrap();
        let key = crate::config::Config::load(&paths.config_file).unwrap().api_key().expect("torbox key");
        let http = reqwest::Client::new();
        let tb = TorBox::new(http.clone(), key);
        let items = tb.list(Kind::Torrent).await.unwrap();
        for c in candidates(&items) {
            match fetch_index(&tb, &http, &c).await {
                Ok(idx) => println!(
                    "{} [{}] {} files, e.g. {:?}",
                    c.title,
                    c.system,
                    idx.files.len(),
                    idx.files.iter().take(3).map(|f| &f.name).collect::<Vec<_>>()
                ),
                Err(e) => println!("{} [{}] ERROR {e:#}", c.title, c.system),
            }
        }
    }

    #[tokio::test]
    #[ignore = "network + TorBox key; downloads the smallest .7z of one set"]
    async fn extract_7z_live() {
        let paths = crate::config::Paths::new().unwrap();
        let key = crate::config::Config::load(&paths.config_file).unwrap().api_key().expect("torbox key");
        let http = reqwest::Client::new();
        let tb = TorBox::new(http.clone(), key);
        let items = tb.list(Kind::Torrent).await.unwrap();
        let c = candidates(&items).into_iter().find(|c| c.title.contains("Sony - PlayStation") && c.title.starts_with("1G1R")).unwrap();
        let item = tb.get(Kind::Torrent, c.id).await.unwrap();
        let url = tb.request_dl(Kind::Torrent, c.id, item.files[0].id).await.unwrap();
        let entries = crate::remotezip::list(&http, &url).await.unwrap();
        let e = entries.iter().filter(|e| has_ext(&e.name, "7z") && e.method == 0).min_by_key(|e| e.size).unwrap();
        let dir = std::env::temp_dir().join(format!("romgetter-tbzip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join(e.base_name());
        crate::remotezip::extract(&http, &url, e, &dest, &|_, _| {}).await.unwrap();
        let out = crate::vimm::extract_7z(&dest, &dir, &|_, _| {}).unwrap();
        println!("{} ({} bytes) -> {out:?}", e.name, e.size);
        std::fs::remove_dir_all(&dir).ok();
        assert!(!out.is_empty(), "nothing unpacked");
    }
}
