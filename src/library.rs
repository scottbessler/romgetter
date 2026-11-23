//! The user's game library: things they've picked, sent to TorBox, and maybe downloaded.

use crate::jobs::sanitize;
use crate::torbox::Kind;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// Direct/hoster URL → TorBox web download
    Link,
    /// Magnet → TorBox torrent
    Magnet,
    /// Found in the roms folder; not from any source we know. Never sent to TorBox.
    Local,
    /// A Vimm's Lair page (`source`): downloaded in a browser, then picked up from the
    /// downloads folder. Never sent to TorBox.
    Vimm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    /// The source URL / magnet; unique per game.
    pub id: String,
    pub name: String,
    /// RetroDECK system folder; empty = detect from the downloaded files.
    #[serde(default)]
    pub system: String,
    pub kind: SourceKind,
    pub source: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub torbox_id: Option<u64>,
    /// Real-Debrid torrent id, for a Minerva game fetched through Real-Debrid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rd_id: Option<String>,
    /// Files saved locally, relative to roms_dir (e.g. "snes/Super Mario World (USA).zip").
    #[serde(default)]
    pub local_files: Vec<String>,
    /// For one game inside a big torrent (Minerva): the file to download, and its index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_index: Option<u32>,
    #[serde(default)]
    pub added: u64,
    /// A readable title when the file name isn't one (IF Archive games).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Game {
    pub fn new(name: String, system: String, kind: SourceKind, source: String, size: u64) -> Self {
        let added = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Self {
            id: source.clone(),
            name,
            system,
            kind,
            source,
            size,
            torbox_id: None,
            rd_id: None,
            local_files: Vec::new(),
            pick: None,
            torrent_index: None,
            added,
            title: None,
        }
    }

    /// One file out of a (usually huge, usually TorBox-cached) torrent.
    pub fn torrent_file(id: String, name: String, system: String, magnet: String, index: Option<u32>, size: u64) -> Self {
        let mut g = Self::new(name.clone(), system, SourceKind::Magnet, magnet, size);
        g.id = id;
        g.pick = Some(name);
        g.torrent_index = index;
        g
    }

    /// A game found in the roms folder (`rel` is relative to roms_dir, e.g. "snes/Zelda.zip").
    pub fn local(system: &str, rel: String, size: u64) -> Self {
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        let mut g = Self::new(name, system.to_string(), SourceKind::Local, String::new(), size);
        g.id = format!("local:{rel}");
        g.local_files = vec![rel];
        g
    }

    /// Paths relative to roms_dir, lowercase, for matching against what's on disk.
    pub fn local_keys(&self, roms_dir: &Path) -> Vec<String> {
        self.local_paths(roms_dir)
            .iter()
            .filter_map(|p| p.strip_prefix(roms_dir).ok().map(|r| r.to_string_lossy().replace('\\', "/").to_lowercase()))
            .collect()
    }

    pub fn btih(&self) -> Option<String> {
        (self.kind == SourceKind::Magnet).then(|| crate::minerva::btih_of(&self.source)).flatten()
    }

    pub fn torbox_kind(&self) -> Kind {
        match self.kind {
            SourceKind::Link | SourceKind::Local | SourceKind::Vimm => Kind::WebDl,
            SourceKind::Magnet => Kind::Torrent,
        }
    }

    /// Where this game lives (or will live) on disk.
    pub fn local_paths(&self, roms_dir: &Path) -> Vec<PathBuf> {
        if !self.local_files.is_empty() {
            return self.local_files.iter().map(|f| roms_dir.join(f)).collect();
        }
        if self.system.is_empty() {
            return Vec::new();
        }
        if let Some(pick) = &self.pick {
            return vec![roms_dir.join(&self.system).join(sanitize(pick))];
        }
        if self.kind == SourceKind::Link {
            return vec![roms_dir.join(&self.system).join(sanitize(&self.name))];
        }
        Vec::new()
    }

    pub fn is_present(&self, roms_dir: &Path) -> bool {
        let paths = self.local_paths(roms_dir);
        // folders count: some systems keep a game as a directory (PS3, multi-disc sets)
        !paths.is_empty() && paths.iter().all(|p| p.exists())
    }
}

/// Seconds since 1970.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether a file name ends in `.ext`, ignoring case ("Game.ZIP" has ext "zip").
pub fn has_ext(name: &str, ext: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Something in the roms folder that looks like a game.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub system: &'static str,
    /// relative to roms_dir, e.g. "snes/Super Mario World (USA).zip"
    pub rel: String,
    pub size: u64,
    /// Files this one points to (a .cue's .bin tracks, a .gdi's tracks, an .m3u's discs),
    /// relative to roms_dir: part of the same game, not games of their own.
    pub tracks: Vec<String>,
}

/// The files a disc sheet (.cue/.gdi/.m3u/.ccd) refers to, as written (relative to it).
pub fn sheet_refs(name: &str, text: &str) -> Vec<String> {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let unquote = |s: &str| s.trim().trim_matches('"').to_string();
    match ext {
        "cue" => text
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                let rest = l.strip_prefix("FILE ").or_else(|| l.strip_prefix("file "))?;
                // FILE "name with spaces.bin" BINARY  /  FILE name.bin BINARY
                Some(match rest.strip_prefix('"') {
                    Some(q) => q.split('"').next().unwrap_or("").to_string(),
                    None => rest.rsplit_once(' ').map(|(n, _)| n).unwrap_or(rest).to_string(),
                })
            })
            .collect(),
        // "1 0 4 2352 track01.bin 0" (the name may be quoted and contain spaces)
        "gdi" => text
            .lines()
            .skip(1)
            .filter_map(|l| {
                let l = l.trim();
                if let Some((_, q)) = l.split_once('"') {
                    return q.split('"').next().map(str::to_string);
                }
                l.split_whitespace().nth(4).map(str::to_string)
            })
            .collect(),
        "m3u" => text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(unquote).collect(),
        "ccd" => {
            let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
            vec![format!("{stem}.img"), format!("{stem}.sub")]
        }
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// Files RetroDECK / ES-DE / emulators keep next to the games.
const NOT_GAMES: &[&str] = &["systeminfo.txt", "metadata.txt", "gamelist.xml", "media", "images", "videos", "manuals", "downloaded_media"];
const NOT_GAME_EXT: &[&str] =
    &["part", "dkey", "key", "txt", "xml", "nfo", "jpg", "jpeg", "png", "srm", "sav", "state", "cfg", "ini", "log", "db"];

/// Every game-looking entry directly inside `roms_dir/<system>/` for known systems.
pub fn scan(roms_dir: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    for (system, _) in crate::systems::SYSTEMS {
        let Ok(dir) = std::fs::read_dir(roms_dir.join(system)) else { continue };
        let mut files: Vec<(String, u64)> = Vec::new();
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            if name.starts_with('.') || NOT_GAMES.contains(&lower.as_str()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_file() && NOT_GAME_EXT.contains(&ext) {
                continue;
            }
            files.push((name, if meta.is_file() { meta.len() } else { 0 }));
        }
        files.sort();
        out.extend(group_tracks(system, &roms_dir.join(system), &files));
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// One folder's files as games: a disc sheet takes the files it points to (and theirs,
/// for an .m3u of .cues) as its tracks; those don't show up as games themselves.
fn group_tracks(system: &'static str, dir: &Path, files: &[(String, u64)]) -> Vec<Found> {
    let by_lower: HashMap<String, usize> = files.iter().enumerate().map(|(i, (n, _))| (n.to_lowercase(), i)).collect();
    let refs: Vec<Vec<usize>> = files
        .iter()
        .map(|(name, size)| {
            let is_sheet = matches!(name.to_lowercase().rsplit_once('.').map(|(_, e)| e), Some("cue" | "gdi" | "m3u" | "ccd"));
            if !is_sheet || *size > 1 << 20 {
                return Vec::new();
            }
            let text = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
            sheet_refs(name, &text)
                .iter()
                .filter_map(|r| by_lower.get(&r.rsplit(['/', '\\']).next().unwrap_or(r).to_lowercase()).copied())
                .collect()
        })
        .collect();
    let member: HashSet<usize> = refs.iter().flatten().copied().collect();
    let mut out = Vec::new();
    for (i, (name, size)) in files.iter().enumerate() {
        if member.contains(&i) {
            continue;
        }
        let (mut tracks, mut seen, mut stack) = (Vec::new(), HashSet::from([i]), refs[i].clone());
        while let Some(j) = stack.pop() {
            if seen.insert(j) {
                tracks.push(j);
                stack.extend(refs[j].iter().copied());
            }
        }
        tracks.sort_unstable();
        out.push(Found {
            system,
            rel: format!("{system}/{name}"),
            size: size + tracks.iter().map(|&j| files[j].1).sum::<u64>(),
            tracks: tracks.iter().map(|&j| format!("{system}/{}", files[j].0)).collect(),
        });
    }
    out
}

/// A display name for a pasted link: magnet `dn=`, else the URL's last path segment.
pub fn name_from_link(link: &str) -> String {
    let raw = if let Some(q) = link.strip_prefix("magnet:?") {
        q.split('&').find_map(|kv| kv.strip_prefix("dn=")).map(|v| v.replace('+', " "))
    } else {
        let path = link.split(['?', '#']).next().unwrap_or(link);
        path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty() && !s.contains(':')).map(str::to_string)
    };
    match raw {
        Some(r) => urlencoding::decode(&r).map(std::borrow::Cow::into_owned).unwrap_or(r),
        None => link.chars().take(60).collect(),
    }
}

pub struct Library {
    pub games: Vec<Game>,
    path: PathBuf,
}

impl Library {
    pub fn load(path: PathBuf) -> Result<Self> {
        let games = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { games, path })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.games)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.games.iter().any(|g| g.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&Game> {
        self.games.iter().find(|g| g.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Game> {
        self.games.iter_mut().find(|g| g.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disc_tracks_join_their_sheet() {
        let dir = std::env::temp_dir().join(format!("romgetter-tracks-{}", std::process::id()));
        let ngcd = dir.join("neogeocd");
        std::fs::create_dir_all(&ngcd).unwrap();
        std::fs::write(
            ngcd.join("Samurai Shodown (USA).cue"),
            "FILE \"Samurai Shodown (USA) (Track 01).bin\" BINARY\n  TRACK 01 MODE1/2352\nFILE \"Samurai Shodown (USA) (Track 02).bin\" BINARY\n",
        )
        .unwrap();
        std::fs::write(ngcd.join("Samurai Shodown (USA) (Track 01).bin"), [0u8; 10]).unwrap();
        std::fs::write(ngcd.join("Samurai Shodown (USA) (Track 02).bin"), [0u8; 5]).unwrap();
        std::fs::write(ngcd.join("Other.chd"), [0u8; 3]).unwrap();
        std::fs::create_dir_all(dir.join("dreamcast")).unwrap();
        std::fs::write(dir.join("dreamcast/Game.gdi"), "2\n1 0 4 2352 track01.bin 0\n2 756 0 2352 \"track 02.raw\" 0\n").unwrap();
        std::fs::write(dir.join("dreamcast/track01.bin"), [0u8; 4]).unwrap();
        std::fs::write(dir.join("dreamcast/track 02.raw"), [0u8; 4]).unwrap();
        let found = scan(&dir);
        let rels: Vec<&str> = found.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["dreamcast/Game.gdi", "neogeocd/Other.chd", "neogeocd/Samurai Shodown (USA).cue"]);
        let cue = &found[2];
        assert_eq!(cue.tracks.len(), 2);
        assert_eq!(cue.size, cue_len(&ngcd) + 15);
        assert_eq!(found[0].tracks, ["dreamcast/track 02.raw", "dreamcast/track01.bin"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn cue_len(dir: &Path) -> u64 {
        std::fs::metadata(dir.join("Samurai Shodown (USA).cue")).unwrap().len()
    }

    #[test]
    fn names_from_links() {
        assert_eq!(name_from_link("magnet:?xt=urn:btih:abc&dn=Chrono+Trigger+%28USA%29&tr=x"), "Chrono Trigger (USA)");
        assert_eq!(name_from_link("https://h/x/Metroid%20Fusion.gba?t=1"), "Metroid Fusion.gba");
    }

    #[test]
    fn scans_roms_folder() {
        let dir = std::env::temp_dir().join(format!("romgetter-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("snes")).unwrap();
        std::fs::create_dir_all(dir.join("ps3/God of War III.ps3")).unwrap();
        std::fs::create_dir_all(dir.join("notasystem")).unwrap();
        for f in ["snes/Zelda (USA).zip", "snes/systeminfo.txt", "snes/.hidden", "snes/half.zip.part", "notasystem/x.zip"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let found = scan(&dir);
        let rels: Vec<&str> = found.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["ps3/God of War III.ps3", "snes/Zelda (USA).zip"]);
        let g = Game::local("snes", "snes/Zelda (USA).zip".into(), 1);
        assert!(g.is_present(&dir));
        assert_eq!(g.local_keys(&dir), vec!["snes/zelda (usa).zip"]);
        assert!(Game::local("ps3", "ps3/God of War III.ps3".into(), 0).is_present(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn presence_uses_expected_path() {
        let dir = std::env::temp_dir().join(format!("romgetter-test-{}", std::process::id()));
        let mut g = Game::new("A Game.zip".into(), "snes".into(), SourceKind::Link, "https://x/a".into(), 1);
        assert!(!g.is_present(&dir));
        std::fs::create_dir_all(dir.join("snes")).unwrap();
        std::fs::write(dir.join("snes/A Game.zip"), b"x").unwrap();
        assert!(g.is_present(&dir));
        g.local_files = vec!["gba/other.gba".into()];
        assert!(!g.is_present(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
