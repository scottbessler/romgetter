//! ROM-set torrents already in the TorBox account (e.g. the "1G1R - Redump - Sony -
//! PlayStation" torrents listed on the Emulation General wiki). Each becomes a
//! searchable set. TorBox packs a torrent with 100+ files into one zip: its central
//! directory is read over HTTP range requests, and a game is later pulled out of the zip
//! by `jobs.rs` exactly like a zipped Minerva folder. Smaller torrents keep their files
//! loose, and TorBox's file list is the index.

use crate::archive::{ItemIndex, RomFile};
use crate::library::has_ext;
use crate::torbox::{Item, Kind, RemoteFile, TorBox};
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
    /// The games are in this file of a loose torrent: a collection zip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner: Option<u64>,
}

fn yes() -> bool {
    true
}

impl Candidate {
    pub fn key(&self) -> String {
        set_key(&self.hash)
    }

    /// Holds several systems, one per folder (a torrent named just "REDUMP").
    pub fn mixed(&self) -> bool {
        self.system.is_empty()
    }
}

/// The folder name a torrent's files sit in; TorBox's item name is sometimes just the hash.
fn folder_name(item: &Item) -> String {
    let path = item.files.first().map(|f| f.name.as_str()).unwrap_or("");
    let folder = path.split('/').next().filter(|f| !f.is_empty() && *f != path).unwrap_or(&item.name);
    folder.replace("&amp;", "&")
}

/// How a set torrent's games are laid out on TorBox.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Layout {
    /// TorBox packed the whole torrent into one zip.
    Zipped,
    /// Loose game files.
    Loose,
    /// One big collection zip among the torrent's files (this file id).
    Inner(u64),
}

/// Whether a torrent is a usable ROM set: a big zip, a few loose games, or a loose
/// torrent around one big collection zip.
fn layout(i: &Item) -> Option<Layout> {
    if i.is_zipped() {
        return (i.files[0].size >= MIN_SIZE && has_ext(&i.files[0].name, "zip")).then_some(Layout::Zipped);
    }
    if i.files.iter().filter(|f| is_game(&f.name)).count() >= MIN_LOOSE_GAMES {
        return Some(Layout::Loose);
    }
    i.inner_zips().next().map(|z| Layout::Inner(z.id))
}

/// A name that says ROM set without saying which system ("REDUMP", "No-Intro 2024").
fn says_rom_set(title: &str) -> bool {
    let t: String = title.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    ["redump", "nointro", "tosec", "1g1r", "romset", "roms", "myrient"].iter().any(|w| t.contains(w))
}

/// The system of one game file of a mixed set, from its extension or folders.
fn file_system(path: &str) -> Option<&'static str> {
    crate::systems::guess_for_file(path)
}

/// Cached torrents whose name says which system they hold, or that hold several systems
/// in folders named after them. Minerva folders are skipped: they're indexed from
/// Minerva's own listings (with torrent indices).
pub fn candidates(items: &[Item]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = items
        .iter()
        .filter(|i| (i.download_present || i.download_finished) && !i.files.is_empty())
        .filter(|i| !i.name.starts_with("Minerva") && !i.files.iter().any(|f| f.name.starts_with("Minerva_Myrient/")))
        .filter_map(|i| {
            let layout = layout(i)?;
            let title = folder_name(i);
            // PC-style repacks (installer chunks, not ROMs)
            if ["repack", "fitgirl", "dodi"].iter().any(|w| title.to_lowercase().contains(w)) {
                return None;
            }
            let system = match crate::systems::guess_from_title(&title) {
                Some(s) => s.to_string(),
                // several systems: loose files show theirs now; a zip's show once it's read
                None if layout == Layout::Loose => {
                    let known = i.files.iter().filter(|f| is_game(&f.name) && file_system(&f.name).is_some()).count();
                    (known >= MIN_LOOSE_GAMES).then(String::new)?
                }
                None => says_rom_set(&title).then(String::new)?,
            };
            let inner = match layout {
                Layout::Inner(id) => Some(id),
                _ => None,
            };
            Some(Candidate { id: i.id, hash: i.hash.to_lowercase(), system, title, zipped: layout == Layout::Zipped, inner })
        })
        .collect();
    out.sort_by(|a, b| a.hash.cmp(&b.hash));
    out.dedup_by(|a, b| a.hash == b.hash);
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// What each torrent in the account holds, by lowercase info-hash: None = all of it (a
/// zip TorBox packed), Some(files) = these files (lowercase base names). Loose torrents
/// list their files, since one added with only some files selected looks complete.
pub fn held(items: &[Item]) -> std::collections::HashMap<String, Option<std::collections::HashSet<String>>> {
    items
        .iter()
        .filter(|i| (i.download_present || i.download_finished) && !i.hash.is_empty())
        .map(|i| {
            let files = (!i.is_zipped())
                .then(|| i.files.iter().filter(|f| !has_ext(&f.name, "parts")).map(|f| f.file_name().to_lowercase()).collect());
            (i.hash.to_lowercase(), files)
        })
        .collect()
}

/// Torrents in the account TorBox is still downloading: lowercase info-hash → (state,
/// progress 0..1).
pub fn pending(items: &[Item]) -> std::collections::HashMap<String, (String, f64)> {
    items
        .iter()
        .filter(|i| !i.download_present && !i.download_finished && !i.hash.is_empty())
        .map(|i| {
            (i.hash.to_lowercase(), (if i.download_state.is_empty() { "queued".into() } else { i.download_state.clone() }, i.progress))
        })
        .collect()
}

/// Files in these sets that aren't games (metadata, extras, video riding along).
fn is_extra(name: &str) -> bool {
    const EXTS: &[&str] = &[
        "sqlite", "xml", "torrent", "pdf", "exe", "dat", "parts", "mkv", "mp4", "avi", "srt", "nzb", "log", "json", "ini", "bat", "cmd",
        "sh",
    ];
    let lower = name.to_lowercase();
    crate::jobs::is_junk(name)
        || EXTS.iter().any(|e| has_ext(name, e))
        // a set's catalog riding along ("CUE sheets USA.7z")
        || lower.starts_with("cue sheets")
        || lower.starts_with("cuesheets")
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
    let zip = if item.is_zipped() { item.files.first() } else { c.inner.and_then(|id| item.files.iter().find(|f| f.id == id)) };
    let files = match zip {
        Some(z) => zip_files(tb, http, &item, z, c).await?,
        None if c.inner.is_some() => bail!("{}'s collection zip is gone from TorBox", c.title),
        None => loose_files(&item, c.mixed()),
    };
    if c.mixed() && files.is_empty() {
        bail!("no folder of {} names a system", c.title);
    }
    Ok(ItemIndex {
        restricted: false,
        files,
        system: (!c.mixed()).then(|| c.system.clone()),
        // also TorBox's display name for the torrent (see App::minerva_name)
        title: c.title.clone(),
        fetched: crate::library::now_secs(),
        btih: Some(c.hash.clone()),
    })
}

/// A mixed set's file keeps the system its path names, else the set's `hint`; files
/// with neither are dropped.
fn with_system(mixed: bool, path: &str, f: RomFile, hint: Option<&'static str>) -> Option<RomFile> {
    if !mixed {
        return Some(f);
    }
    let system = file_system(path).or(hint)?;
    Some(RomFile { system: Some(system.to_string()), ..f })
}

/// A loose torrent's game files. File ids double as torrent indices unless TorBox has
/// only part of the torrent (then they're renumbered).
fn loose_files(item: &Item, mixed: bool) -> Vec<RomFile> {
    let partial = item.is_partial();
    item.files
        .iter()
        .filter(|f| f.size > 0.0 && is_game(&f.name))
        .filter_map(|f| {
            let rom = RomFile {
                name: f.file_name(),
                size: f.size as u64,
                url: None,
                torrent_index: (!partial).then(|| u32::try_from(f.id).ok()).flatten(),
                title: None,
                system: None,
            };
            with_system(mixed, &f.name, rom, None)
        })
        .collect()
}

/// A system named by a set's catalog files ("REDUMP/ReDump PS2 USA.dat"), for games
/// sitting in a folder that names none ("REDUMP/GAMES/").
fn catalog_system<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    paths.into_iter().filter(|p| ["dat", "xml", "txt"].iter().any(|e| has_ext(p, e))).find_map(|p| {
        let base = p.rsplit('/').next().unwrap_or(p);
        crate::systems::guess_from_title(base.rsplit_once('.').map_or(base, |(stem, _)| stem))
    })
}

async fn zip_files(tb: &TorBox, http: &reqwest::Client, item: &Item, zip: &RemoteFile, c: &Candidate) -> Result<Vec<RomFile>> {
    let url = tb.request_dl(Kind::Torrent, item.id, zip.id).await?;
    let claimed = zip.size as u64;
    let (entries, served) =
        crate::remotezip::list_sized(http, &url).await.map_err(|e| anyhow!("TorBox's zip of {} can't be read: {e:#}", c.title))?;
    // TorBox sometimes serves only the start of a zip (with a directory of just that
    // part): the games it holds are fine, the rest aren't there.
    let served = served.unwrap_or(claimed);
    let hint = if c.mixed() { catalog_system(entries.iter().map(|e| e.name.as_str())) } else { None };
    let files: Vec<RomFile> = entries
        .iter()
        .filter(|e| crate::remotezip::is_supported(e.method) && e.size > 0 && is_game(&e.name) && e.ends_by(served))
        .filter_map(|e| {
            let rom = RomFile { name: e.base_name().to_string(), size: e.size, url: None, torrent_index: None, title: None, system: None };
            with_system(c.mixed(), &e.name, rom, hint)
        })
        .collect();
    if files.is_empty() && served < claimed {
        bail!("TorBox's zip of {} is cut off ({} of {})", c.title, crate::ui::human(served), crate::ui::human(claimed));
    }
    Ok(files)
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
            // a collection zip inside a loose torrent
            item(serde_json::json!({"id": 9, "hash": "99", "name": "NES_Test_Collection_3", "download_present": true,
                "files": [{"id": 2, "name": "NES_Test_Collection_3/NES.zip", "size": 1.2e9}, {"id": 0, "name": "NES_Test_Collection_3/meta.sqlite", "size": 1e5},
                          {"id": 1, "name": "NES_Test_Collection_3/meta.xml", "size": 1e5}]})),
            // several systems, by folder: packed (read later) and loose
            item(serde_json::json!({"id": 10, "hash": "1010", "name": "REDUMP", "download_present": true,
                "files": [{"id": 0, "name": "REDUMP/REDUMP.zip", "size": 7e11, "zipped": true}]})),
            item(serde_json::json!({"id": 11, "hash": "1111", "name": "REDUMP", "download_present": true,
                "files": [{"id": 0, "name": "REDUMP/Sony - PlayStation/a.chd", "size": 1e8}, {"id": 1, "name": "REDUMP/Sega - Saturn/b.chd", "size": 1e8},
                          {"id": 2, "name": "REDUMP/Sega - Saturn/c.chd", "size": 1e8}, {"id": 3, "name": "REDUMP/Extras/d.chd", "size": 1e8}]})),
            // a big zip that says nothing about games
            item(serde_json::json!({"id": 12, "hash": "1212", "name": "Holiday Photos", "download_present": true,
                "files": [{"id": 0, "name": "Holiday Photos/all.zip", "size": 9e9}, {"id": 1, "name": "Holiday Photos/x.txt", "size": 1e3}]})),
            item(serde_json::json!({"id": 13, "hash": "1313", "name": "eXoDOSRPG", "download_present": true,
                "files": [{"id": 0, "name": "eXoDOSRPG/eXoDOSRPG.zip", "size": 2e10, "zipped": true}]})),
        ];
        let got = candidates(&items);
        let sys: Vec<_> = got.iter().map(|c| (c.id, c.system.as_str(), c.zipped, c.inner)).collect();
        assert_eq!(
            sys,
            vec![
                (5, "atarijaguar", false, None),
                (1, "gc", true, None),
                (4, "psp", true, None),
                (9, "nes", false, Some(2)),
                (10, "", true, None),
                (11, "", false, None),
                (13, "dos", true, None),
            ],
            "{got:?}"
        );
        assert!(!is_game("REDUMP/CUE sheets USA.7z") && is_game("REDUMP/GAMES/Jackass - The Game (USA).7z"));
        assert_eq!(catalog_system(["REDUMP/GAMES/x.7z", "REDUMP/GAMES MD5.txt", "REDUMP/ReDump PS2 USA.dat"]), Some("ps2"));
        let mixed: Vec<_> = loose_files(&items[10], true).into_iter().map(|f| (f.name, f.system)).collect();
        assert_eq!(
            mixed,
            vec![("a.chd".into(), Some("psx".into())), ("b.chd".into(), Some("saturn".into())), ("c.chd".into(), Some("saturn".into()))]
        );
        let names: Vec<_> = loose_files(&items[4], false).into_iter().map(|f| (f.name, f.torrent_index)).collect();
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
