//! Vimm's Lair (vimm.net/vault). Its search pages are plain HTML, but every game page and
//! download sits behind a Cloudflare human check, so romgetter can't download from it.
//! Instead: open the game's page in a real browser, let the person press Download, then
//! notice the file in the downloads folder and move it into `roms/<system>/`.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const BASE: &str = "https://vimm.net";

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: u64,
    pub title: String,
    /// RetroDECK folder.
    pub system: &'static str,
    pub regions: Vec<String>,
}

impl Hit {
    /// "Chrono Trigger (USA)", close to the file name Vimm serves.
    pub fn name(&self) -> String {
        if self.regions.is_empty() { self.title.clone() } else { format!("{} ({})", self.title, self.regions.join(", ")) }
    }

    pub fn url(&self) -> String {
        page_url(self.id)
    }
}

pub fn page_url(id: u64) -> String {
    format!("{BASE}/vault/{id}")
}

pub fn is_vimm_url(url: &str) -> bool {
    url.starts_with(&format!("{BASE}/vault/"))
}

/// Vimm's system codes (its search results' first column) → RetroDECK folders.
pub fn system_for(code: &str) -> Option<&'static str> {
    Some(match code {
        "NES" => "nes",
        "SNES" => "snes",
        "N64" => "n64",
        "GameCube" => "gc",
        "Wii" | "WiiWare" => "wii",
        "WiiU" => "wiiu",
        "GB" => "gb",
        "GBC" => "gbc",
        "GBA" => "gba",
        "DS" => "nds",
        "3DS" => "n3ds",
        "VB" => "virtualboy",
        "PS1" => "psx",
        "PS2" => "ps2",
        "PS3" => "ps3",
        "PSP" => "psp",
        "Xbox" => "xbox",
        "Xbox360" => "xbox360",
        "SMS" => "mastersystem",
        "GG" => "gamegear",
        "Genesis" => "megadrive",
        "SegaCD" => "segacd",
        "32X" => "sega32x",
        "Saturn" => "saturn",
        "Dreamcast" => "dreamcast",
        "Atari2600" => "atari2600",
        "Atari5200" => "atari5200",
        "Atari7800" => "atari7800",
        "Lynx" => "atarilynx",
        "Jaguar" => "atarijaguar",
        "JaguarCD" => "atarijaguarcd",
        "TG16" => "tg16",
        "TGCD" => "pcenginecd",
        "CDi" => "cdimono1",
        _ => return None,
    })
}

pub async fn search(http: &reqwest::Client, q: &str) -> Result<Vec<Hit>> {
    let html = http.get(format!("{BASE}/vault/")).query(&[("p", "list"), ("q", q)]).send().await?.error_for_status()?.text().await?;
    Ok(parse_list(&html))
}

/// Rows of a `?p=list` results table: `<tr><td>SYS</td><td>…<a href="/vault/ID">Title</a>…</td><td>flags</td>…`.
pub fn parse_list(html: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    for row in html.split("<tr>").skip(1) {
        let row = row.split("</tr>").next().unwrap_or(row);
        let cells: Vec<&str> = row.split("<td").skip(1).collect();
        if cells.len() < 3 {
            continue;
        }
        let Some(system) = system_for(&cell_text(cells[0])) else { continue };
        let Some((id, title)) = vault_link(cells[1]) else { continue };
        let regions = cells[2].split("title=\"").skip(1).filter_map(|s| s.split('"').next()).map(decode).collect();
        out.push(Hit { id, title, system, regions });
    }
    out
}

fn vault_link(cell: &str) -> Option<(u64, String)> {
    let rest = cell.split("href=\"/vault/").nth(1)?;
    let (id, rest) = rest.split_once('"')?;
    let id = id.parse().ok()?;
    let title = rest.split_once('>')?.1.split("</a>").next()?;
    Some((id, decode(title.trim())))
}

fn cell_text(cell: &str) -> String {
    let inner = cell.split_once('>').map(|(_, r)| r).unwrap_or(cell);
    decode(inner.split('<').next().unwrap_or("").trim())
}

fn decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

// ---------- browser ----------

/// Open `url` in a browser that can save downloads. `custom` (config `browser`) is a
/// command; the URL is appended. Otherwise: Firefox or Chrome (Flatpak, as on a Steam
/// Deck), then the desktop's default. Steam's built-in browser can't save files, which is
/// what `xdg-open` reaches from Game Mode, so a real browser goes first.
pub fn open_in_browser(url: &str, custom: Option<&str>) -> Result<String> {
    let mut candidates: Vec<Vec<String>> = Vec::new();
    if let Some(cmd) = custom.filter(|c| !c.trim().is_empty()) {
        candidates.push(cmd.split_whitespace().map(String::from).collect());
    }
    if cfg!(target_os = "macos") {
        candidates.push(vec!["open".into()]);
    } else {
        for app in ["org.mozilla.firefox", "com.google.Chrome", "org.chromium.Chromium"] {
            if flatpak_has(app) {
                candidates.push(vec!["flatpak".into(), "run".into(), app.into()]);
            }
        }
        for bin in ["firefox", "google-chrome", "chromium"] {
            candidates.push(vec![bin.into()]);
        }
        candidates.push(vec!["xdg-open".into()]);
    }
    let mut last_err = None;
    for c in candidates {
        let (prog, args) = c.split_first().unwrap();
        match std::process::Command::new(prog)
            .args(args)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
                return Ok(c.last().unwrap().rsplit('.').next().unwrap_or(prog).to_string());
            }
            Err(e) => last_err = Some(format!("{prog}: {e}")),
        }
    }
    bail!("couldn't start a browser ({})", last_err.unwrap_or_default())
}

fn flatpak_has(app: &str) -> bool {
    ["/var/lib/flatpak/app", "~/.local/share/flatpak/app"].iter().any(|d| {
        let d = d.strip_prefix("~/").map(|r| home().join(r)).unwrap_or_else(|| PathBuf::from(d));
        d.join(app).is_dir()
    })
}

fn home() -> PathBuf {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()).unwrap_or_default()
}

pub fn default_downloads_dir() -> PathBuf {
    directories::UserDirs::new().and_then(|u| u.download_dir().map(Path::to_path_buf)).unwrap_or_else(|| home().join("Downloads"))
}

// ---------- downloads folder ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

/// Files the browser has finished saving: skips in-progress downloads (Firefox writes
/// `x.zip.part` next to an empty `x.zip`; Chrome writes `x.zip.crdownload`).
pub fn finished_downloads(dir: &Path) -> Vec<Download> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let entries: Vec<(PathBuf, std::fs::Metadata)> =
        rd.flatten().filter_map(|e| e.metadata().ok().map(|m| (e.path(), m))).filter(|(_, m)| m.is_file()).collect();
    let names: std::collections::HashSet<String> =
        entries.iter().filter_map(|(p, _)| p.file_name().map(|n| n.to_string_lossy().to_lowercase())).collect();
    entries
        .into_iter()
        .filter(|(p, m)| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let partial = [".part", ".crdownload", ".download", ".tmp", ".partial"].iter().any(|s| name.ends_with(s))
                || names.contains(&format!("{name}.part"))
                || names.contains(&format!("{name}.crdownload"));
            m.len() > 0 && !partial && !name.starts_with('.')
        })
        .map(|(path, m)| Download { path, size: m.len(), modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH) })
        .collect()
}

/// A file that could be a game (archive or image), for matching by time alone.
pub fn looks_like_game(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "zip"
            | "7z"
            | "iso"
            | "chd"
            | "rvz"
            | "wbfs"
            | "cso"
            | "bin"
            | "cue"
            | "nes"
            | "sfc"
            | "smc"
            | "z64"
            | "n64"
            | "gb"
            | "gbc"
            | "gba"
            | "nds"
            | "3ds"
            | "md"
            | "gen"
            | "sms"
            | "gg"
    )
}

/// A title reduced for matching: no extension, no "(USA)"/"[!]" tags, no "The", only
/// letters and digits. "Legend of Zelda, The - A Link to the Past (USA).zip" and "The
/// Legend of Zelda: A Link to the Past" agree.
pub fn norm(s: &str) -> String {
    let s = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let s = match s.rsplit_once('.') {
        Some((stem, ext)) if ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => stem,
        _ => s,
    };
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let lower = out.to_lowercase().replace('&', " and ");
    lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && *w != "the").collect()
}

/// The parenthesized tags of a name, lowercase: `(USA, Europe) (Rev 1)` → `["usa, europe", "rev 1"]`.
pub fn tags(s: &str) -> Vec<String> {
    s.split('(').skip(1).filter_map(|p| p.split(')').next()).map(|t| t.trim().to_lowercase()).collect()
}

/// How well a downloaded file fits a library game: None = not this game; higher = better.
pub fn match_score(game_name: &str, file: &str) -> Option<u32> {
    let (g, f) = (norm(game_name), norm(file));
    if g.is_empty() || g != f {
        return None;
    }
    let gt = tags(game_name);
    let ft = tags(file);
    Some(1 + gt.iter().filter(|t| ft.contains(t)).count() as u32)
}

// ---------- import ----------

/// Move a finished download into `roms/<system>/`. A .7z is unpacked there (few
/// emulators read 7z); a .zip is moved as-is and left to the usual unzip rules.
/// Returns the saved files relative to `roms_dir`. Blocking.
pub fn import(src: &Path, system: &str, roms_dir: &Path, progress: &dyn Fn(u64, u64)) -> Result<Vec<String>> {
    if system.is_empty() {
        bail!("no system selected");
    }
    let dir = roms_dir.join(system);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = crate::jobs::sanitize(&src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    if name.to_lowercase().ends_with(".7z") {
        let files = extract_7z(src, &dir, progress)?;
        std::fs::remove_file(src).ok();
        return Ok(files.into_iter().map(|f| format!("{system}/{f}")).collect());
    }
    let dest = dir.join(&name);
    move_file(src, &dest, progress)?;
    Ok(vec![format!("{system}/{name}")])
}

/// Rename, or copy + delete across filesystems (Downloads on the internal drive, roms on an SD card).
fn move_file(src: &Path, dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<()> {
    if std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    let total = std::fs::metadata(src)?.len();
    let part = dest.with_extension(format!("{}.part", dest.extension().map(|e| e.to_string_lossy()).unwrap_or_default()));
    {
        let mut r = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
        let mut w = std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
        copy_with_progress(&mut r, &mut w, total, progress)?;
    }
    std::fs::rename(&part, dest)?;
    std::fs::remove_file(src).ok();
    Ok(())
}

fn copy_with_progress(r: &mut dyn std::io::Read, w: &mut dyn std::io::Write, total: u64, progress: &dyn Fn(u64, u64)) -> Result<u64> {
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    Ok(done)
}

/// Extract every file of a .7z into `dir`, flattened to base names (Vimm's archives hold a
/// game's files at the top level). Returns the file names.
fn extract_7z(src: &Path, dir: &Path, progress: &dyn Fn(u64, u64)) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut unpacked = 0u64;
    let mut err = None;
    sevenz_rust2::decompress_file_with_extract_fn(src, dir, |entry, reader, _| {
        // Vimm adds a "Vimm's Lair.txt" to its archives; it isn't part of the game.
        if entry.is_directory() || crate::jobs::is_junk(entry.name()) {
            return Ok(true);
        }
        let name = crate::jobs::sanitize(entry.name());
        let dest = dir.join(&name);
        let part = dir.join(format!("{name}.part"));
        let res = (|| -> Result<()> {
            let mut w = std::fs::File::create(&part)?;
            let size = entry.size();
            copy_with_progress(reader, &mut w, size, &|d, _| progress(unpacked + d, 0))?;
            unpacked += size;
            std::fs::rename(&part, &dest)?;
            Ok(())
        })();
        if let Err(e) = res {
            std::fs::remove_file(&part).ok();
            err = Some(e);
            return Ok(false);
        }
        names.push(name);
        Ok(true)
    })
    .map_err(|e| anyhow::anyhow!("unpacking {}: {e}", src.display()))?;
    if let Some(e) = err {
        return Err(e.context(format!("unpacking {}", src.display())));
    }
    if names.is_empty() {
        bail!("{} was empty", src.display());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: &str = r#"<table><tr><td style="width:80px; text-align:center">SNES</td><td style="width:auto"><img src="/images/icons/cart.svg" class="media-icon" title="Cartridge"><a href="/vault/1127" onmouseover="buildTooltip(this, 1127, 256, 222)">Chrono Trigger</a>&nbsp; <a href="/manual/4301"><img src="/images/manual_1.gif" title="Read the manual"></a></td><td style="width:65px; text-align:center"><div><img src="/images/flags/usa.png" class="flag" title="USA"></div></td><td>1.0</td></tr>
<tr><td style="width:80px; text-align:center">PS1</td><td style="width:auto"><img src="/images/icons/disc.svg" class="media-icon" title="Disc"><a href="/vault/2833">Final Fantasy Chronicles: Chrono Trigger &amp; More</a></td><td><div><img class="flag" title="USA"><img class="flag" title="Canada"></div></td></tr>
<tr><td>Commodore64</td><td><a href="/vault/9">Unmapped</a></td><td></td></tr></table>"#;

    #[tokio::test]
    #[ignore = "network"]
    async fn live_search() {
        let http = reqwest::Client::builder().user_agent("romgetter-test").build().unwrap();
        let hits = search(&http, "chrono trigger").await.unwrap();
        assert!(hits.iter().any(|h| h.system == "snes" && h.title == "Chrono Trigger" && h.regions == ["USA"]), "{hits:#?}");
        for h in hits.iter().take(8) {
            println!("{:<6} {:>6} {}", h.system, h.id, h.name());
        }
    }

    #[test]
    fn parses_search_rows() {
        let hits = parse_list(ROWS);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0], Hit { id: 1127, title: "Chrono Trigger".into(), system: "snes", regions: vec!["USA".into()] });
        assert_eq!(hits[1].title, "Final Fantasy Chronicles: Chrono Trigger & More");
        assert_eq!(hits[1].system, "psx");
        assert_eq!(hits[1].name(), "Final Fantasy Chronicles: Chrono Trigger & More (USA, Canada)");
        assert_eq!(hits[1].url(), "https://vimm.net/vault/2833");
    }

    #[test]
    fn matches_downloaded_names() {
        assert_eq!(norm("Legend of Zelda, The - A Link to the Past (USA).zip"), norm("The Legend of Zelda: A Link to the Past (USA)"));
        assert!(match_score("NFL Blitz Pro (USA)", "NFL Blitz Pro (USA).zip").is_some());
        assert!(match_score("Chrono Trigger (USA)", "Chrono Cross (USA).7z").is_none());
        // region breaks ties between two copies of a title
        let us = match_score("Chrono Trigger (USA)", "Chrono Trigger (USA).zip").unwrap();
        let jp = match_score("Chrono Trigger (Japan)", "Chrono Trigger (USA).zip").unwrap();
        assert!(us > jp);
    }

    #[test]
    fn skips_unfinished_downloads() {
        let dir = std::env::temp_dir().join(format!("romgetter-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Done (USA).zip"), b"x").unwrap();
        std::fs::write(dir.join("Going (USA).zip"), b"").unwrap();
        std::fs::write(dir.join("Going (USA).zip.part"), b"xx").unwrap();
        std::fs::write(dir.join("Chrome (USA).zip.crdownload"), b"xx").unwrap();
        let got: Vec<String> =
            finished_downloads(&dir).iter().map(|d| d.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(got, vec!["Done (USA).zip"]);

        // import moves it into roms/<system>/
        let roms = dir.join("roms");
        let files = import(&dir.join("Done (USA).zip"), "snes", &roms, &|_, _| {}).unwrap();
        assert_eq!(files, vec!["snes/Done (USA).zip"]);
        assert!(roms.join("snes/Done (USA).zip").is_file());
        assert!(!dir.join("Done (USA).zip").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A .7z (LZMA2, as Vimm serves disc games) is unpacked into the system folder and removed.
    #[test]
    fn unpacks_7z() {
        let dir = std::env::temp_dir().join(format!("romgetter-7z-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("Game (USA).7z");
        std::fs::write(&src, include_bytes!("testdata/vimm-disc.7z")).unwrap();
        let roms = dir.join("roms");
        let mut files = import(&src, "psx", &roms, &|_, _| {}).unwrap();
        files.sort();
        assert_eq!(files, vec!["psx/Game (USA) (Track 1).bin", "psx/Game (USA).cue"]);
        assert_eq!(std::fs::metadata(roms.join("psx/Game (USA) (Track 1).bin")).unwrap().len(), 200_000);
        assert!(std::fs::read_to_string(roms.join("psx/Game (USA).cue")).unwrap().starts_with("FILE \"Game (USA) (Track 1).bin\""));
        assert!(!src.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
