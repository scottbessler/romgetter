//! Texture packs for the GameCube (Dolphin) and N64 (RetroArch's Mupen64Plus-Next) games on disk.
//!
//! The catalog merges three lists, deduped by game and author:
//! - EmulationKing (GameCube and N64 pages, files on its own server),
//! - rapka's dolphin-textures list (MEGA links, keyed by disc ID),
//! - putachi's free GameCube list (Google Drive, MEGA, MediaFire, … or a forum/Patreon page).
//!
//! File hosts (MEGA, Google Drive, MediaFire, Dropbox) go through TorBox first, then
//! Real-Debrid; plain file servers (EmulationKing's), which neither takes, download directly.
//! Pages only a person can use (Patreon, Gumroad, forums) open in a browser.
//!
//! Installed packs live in `texture_packs/romgetter/<pack>/<folder>`; enabling one moves its
//! folder into the emulator's texture folder (`Dolphin/Textures/<disc ID>` or
//! `Mupen64Plus/hires_texture/<ROM name>`), disabling moves it back.

use crate::jobs::{self, Ctx, LocalState};
use crate::torbox::Kind;
use crate::vimm::norm;
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SYSTEMS: [&str; 2] = ["gc", "n64"];
/// How old the cached catalog may get before it's fetched again.
pub const REFRESH: Duration = Duration::from_hours(168);

const EK: &str = "https://emulationking.com";
const EK_LISTS: [(&str, &str); 2] = [("gc", "/nintendo/gamecube/texturepacks/index.md"), ("n64", "/nintendo/n64/texturepacks/index.md")];
const RAPKA: &str = "https://raw.githubusercontent.com/rapka/dolphin-textures/master/PACKS.md";
const PUTACHI: &str = "https://putachi.net/ultimate-gamecube-hd-texture-pack-list/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    /// What tells builds apart: "DDS", "Lite", "Rice Format", "MEGA", …
    pub label: String,
    pub url: String,
    /// 0 = unknown
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pack {
    /// Stable across refreshes: "<system>/<game>/<author>", normalized.
    pub key: String,
    pub system: String,
    pub game: String,
    /// GameCube disc ID (3 or 6 characters) or N64 ROM header name, when the source says.
    #[serde(default)]
    pub game_id: Option<String>,
    pub author: String,
    /// A page about the pack, for people.
    #[serde(default)]
    pub page: Option<String>,
    pub builds: Vec<Build>,
    /// Lists it came from ("EmulationKing", "rapka", "putachi").
    pub sources: Vec<String>,
    /// EmulationKing's name for the game in its URLs ("zeldaocarinaoftime"): cleaner than
    /// the pack titles, which are free-form ("CBFD", "Djipis 2009 Cellshade Ocarina of Time").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// The pack's own title, shown when no author is credited.
    #[serde(default)]
    pub name: String,
    /// One line on what the pack changes and its style.
    #[serde(default)]
    pub summary: String,
    /// The longer write-up, a paragraph per entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about: Vec<String>,
    /// "Complete", "Incomplete", "In Progress", …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// "NTSC-U", "PAL", "GLOBAL", …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

/// How a download link can be fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A plain file anyone can download (debrid services don't take these).
    Direct,
    /// A file host only a debrid service (or a browser) can get through.
    Debrid,
    /// A page a person has to use (Patreon, Gumroad, forum posts).
    Browser,
}

pub fn reach(url: &str) -> Reach {
    let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").trim_start_matches("www.").to_lowercase();
    match host.as_str() {
        "files.emulationking.com" => Reach::Direct,
        "mega.nz" | "mega.co.nz" | "mediafire.com" | "drive.google.com" | "docs.google.com" | "dropbox.com" => Reach::Debrid,
        _ => Reach::Browser,
    }
}

impl Pack {
    /// Builds that can be downloaded (not just opened in a browser).
    pub fn downloadable(&self) -> impl Iterator<Item = (usize, &Build)> {
        self.builds.iter().enumerate().filter(|(_, b)| reach(&b.url) != Reach::Browser)
    }

    /// The build Enter installs. N64: GLideN64's ready-made cache (what Mupen64Plus-Next
    /// loads fastest), else the Rice folder; Glide64 caches are for another plugin.
    /// GameCube: a light build for handhelds if there is one, else DDS (loads faster than
    /// PNG), else the first.
    pub fn default_build(&self) -> Option<usize> {
        let all: Vec<_> = self.downloadable().collect();
        let has = |b: &Build, w: &str| b.label.to_lowercase().contains(w);
        let prefs: &[&str] = if self.system == "n64" { &["gliden64", "rice"] } else { &["deck", "lite", "dds"] };
        prefs
            .iter()
            .find_map(|w| all.iter().find(|i| has(i.1, w)))
            .or_else(|| all.iter().find(|i| !has(i.1, "glide64")))
            .or_else(|| all.first())
            .map(|i| i.0)
    }
}

// ---------- catalog ----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    /// unix seconds
    pub fetched: u64,
    pub packs: Vec<Pack>,
}

fn catalog_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("texture-packs.v3.json") // v3: descriptions
}

pub fn load_catalog(cache_dir: &Path) -> Option<Catalog> {
    serde_json::from_slice(&fs::read(catalog_path(cache_dir)).ok()?).ok()
}

pub fn save_catalog(cache_dir: &Path, c: &Catalog) -> Result<()> {
    fs::create_dir_all(cache_dir)?;
    fs::write(catalog_path(cache_dir), serde_json::to_vec(c)?)?;
    Ok(())
}

async fn get_text(http: &reqwest::Client, url: &str) -> Result<String> {
    Ok(http.get(url).send().await?.error_for_status()?.text().await?)
}

/// Fetch every list (EmulationKing's ~100 pack pages, 4 at a time) and merge them.
pub async fn fetch_catalog(http: &reqwest::Client) -> Result<Catalog> {
    let mut pages = Vec::new();
    for (system, path) in EK_LISTS {
        let md = get_text(http, &format!("{EK}{path}")).await.with_context(|| format!("EmulationKing's {system} list"))?;
        pages.extend(parse_ek_list(&md).into_iter().map(|(text, url, summary)| (system.to_string(), text, url, summary)));
    }
    if pages.is_empty() {
        bail!("EmulationKing's lists had no texture packs (page layout changed?)");
    }
    let mut packs: Vec<Pack> = futures_util::stream::iter(pages)
        .map(|(system, text, url, summary): (String, String, String, String)| async move {
            let html = get_text(http, &url).await.ok()?;
            let EkPage { builds, id, about, status } = parse_ek_page(&html);
            let (author, game) = split_title(&text);
            let slug = url.split("/games/").nth(1).and_then(|r| r.split('/').next()).map(str::to_string);
            let name = text.replace(" Texture Pack", "");
            let mut p = new_pack(&system, &game, &author, id, Some(url), builds, "EmulationKing");
            (p.slug, p.name, p.summary, p.about, p.status) = (slug, name, summary, about, status);
            // e.g. Paper Mario's Wii Virtual Console release: a Dolphin pack, not an N64 one
            let wii = system == "n64" && text.to_lowercase().contains("virtual console");
            (!p.builds.is_empty() && !wii).then_some(p)
        })
        .buffered(4)
        .filter_map(|p| async { p })
        .collect()
        .await;
    // the others are extras: a failure just leaves them out
    if let Ok(md) = get_text(http, RAPKA).await {
        packs.extend(parse_rapka(&md));
    }
    if let Ok(html) = get_text(http, PUTACHI).await {
        packs.extend(parse_putachi(&html));
    }
    Ok(Catalog { fetched: crate::library::now_secs(), packs: merge(packs) })
}

fn new_pack(
    system: &str,
    game: &str,
    author: &str,
    game_id: Option<String>,
    page: Option<String>,
    builds: Vec<Build>,
    source: &str,
) -> Pack {
    Pack {
        key: format!("{system}/{}/{}", norm(game), norm(author)),
        system: system.into(),
        game: game.trim().into(),
        game_id,
        author: author.trim().into(),
        page,
        builds,
        sources: vec![source.into()],
        slug: None,
        name: game.trim().into(),
        summary: String::new(),
        about: Vec::new(),
        status: None,
        region: None,
    }
}

/// `- [Title](https://…/index.md): description` lines → (title, page URL, description).
fn parse_ek_list(md: &str) -> Vec<(String, String, String)> {
    md.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("- [")?;
            let (text, rest) = rest.split_once("](")?;
            let (url, summary) = rest.split_once(')')?;
            let summary = unescape(summary.trim_start_matches(':').trim());
            url.contains("/texturepacks/").then(|| (unescape(text), url.trim_end_matches("index.md").to_string(), summary))
        })
        .collect()
}

/// "coolhardy100's Luigi's Mansion Texture Pack" → ("coolhardy100", "Luigi's Mansion").
fn split_title(text: &str) -> (String, String) {
    let t = text.replace('’', "'");
    let (author, game) = t.split_once("'s ").unwrap_or(("", &t));
    let mut game = game.trim().to_string();
    for suffix in [" Texture Pack and Translation", " HD Texture Pack", " Texture Pack", " HD Retexture", " Retexture", " Textures"] {
        if let Some(g) = game.strip_suffix(suffix) {
            game = g.to_string();
            break;
        }
    }
    (author.trim().to_string(), game)
}

struct EkPage {
    builds: Vec<Build>,
    /// GameCube disc ID its instructions name
    id: Option<String>,
    about: Vec<String>,
    status: Option<String>,
}

/// A pack page's download tiles, disc ID, overview paragraphs and status.
fn parse_ek_page(html: &str) -> EkPage {
    let start = html.find("id=download").unwrap_or(html.len());
    let end = html[start..].find("dl-feed").map_or(html.len(), |e| start + e);
    let sec = &html[start..end];
    let mut builds = Vec::new();
    for tile in sec.split("<a class=\"dl-tile").skip(1) {
        let tag = tile.split('>').next().unwrap_or("");
        let Some(url) = attr(tag, "href") else { continue };
        let title = attr(tag, "title").map(|t| unescape(&t)).unwrap_or_default();
        // "Download X (DDS, 608 MB)": the last parentheses hold the build and its size
        let inner = title.rsplit_once('(').and_then(|(_, r)| r.strip_suffix(')')).unwrap_or("");
        let mut size = 0;
        let mut label = Vec::new();
        for part in inner.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match parse_size(part) {
                Some(s) => size = s,
                None => label.push(part),
            }
        }
        let label = if label.is_empty() { ext_of(&url).to_uppercase() } else { label.join(", ") };
        builds.push(Build { label, url, size });
    }
    let id = html.split("Textures/").skip(1).find_map(|rest| {
        let id: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
        (rest[id.len()..].starts_with('/') && is_disc_id(&id)).then_some(id)
    });
    // <div class=console-overview-text><p>…</p><p>…</p></div>
    let about = html
        .split_once("console-overview-text")
        .and_then(|(_, r)| r.split("</div>").next())
        .map(|o| o.split("<p>").skip(1).map(|p| text_of(p.split("</p>").next().unwrap_or(p))).filter(|t| !t.is_empty()).collect())
        .unwrap_or_default();
    // <span><strong>Complete</strong>Status</span>
    let status = html.split_once("</strong>Status<").and_then(|(before, _)| {
        let s = text_of(before.rsplit_once("<strong>")?.1);
        (!s.is_empty() && s.len() < 40).then_some(s)
    });
    EkPage { builds, id, about, status }
}

/// An attribute's value, quoted or not (`href=https://…` in minified HTML).
fn attr(tag: &str, name: &str) -> Option<String> {
    let i = tag.find(&format!(" {name}="))? + name.len() + 2;
    let rest = &tag[i..];
    let v = match rest.chars().next()? {
        q @ ('"' | '\'') => rest[1..].split(q).next()?,
        _ => rest.split([' ', '>']).next()?,
    };
    Some(v.to_string())
}

fn ext_of(url: &str) -> &str {
    url.rsplit('/').next().and_then(|n| n.rsplit_once('.')).map_or("", |(_, e)| e)
}

/// "608 MB" / "1.06 GB" / "38 MB" → bytes.
fn parse_size(s: &str) -> Option<u64> {
    let (n, unit) = s.trim().split_once(' ')?;
    let n: f64 = n.replace(',', "").parse().ok()?;
    let mult = match unit.trim().to_uppercase().as_str() {
        "KB" => 1 << 10,
        "MB" => 1 << 20,
        "GB" => 1 << 30,
        "TB" => 1u64 << 40,
        _ => return None,
    };
    Some((n * mult as f64) as u64)
}

/// A GameCube disc ID as Dolphin names texture folders: "GLM" or "GLME01".
fn is_disc_id(s: &str) -> bool {
    matches!(s.len(), 3 | 6) && s.starts_with('G') && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// `[GM8E01] Metroid Prime: https://mega.nz/…` lines. N-prefixed IDs are N64 games on the
/// Wii's Virtual Console (Dolphin, not our N64 emulator), so only GameCube ones count.
fn parse_rapka(md: &str) -> Vec<Pack> {
    md.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix('[')?;
            let (id, rest) = rest.split_once(']')?;
            let (game, url) = rest.rsplit_once(": ")?;
            (is_disc_id(id) && url.starts_with("http")).then(|| {
                let build = Build { label: "AI upscale".into(), url: url.trim().into(), size: 0 };
                let mut p = new_pack(
                    "gc",
                    game,
                    "rapka",
                    Some(id.into()),
                    Some("https://github.com/rapka/dolphin-textures".into()),
                    vec![build],
                    "rapka",
                );
                p.summary = "The game's own textures upscaled by AI (ESRGAN), not redrawn by hand.".into();
                p
            })
        })
        .collect()
}

/// The free table: title, region, download link(s), size, author.
fn parse_putachi(html: &str) -> Vec<Pack> {
    // the first table is the free one; the paid list comes after a "Paid …" heading
    let paid = html.find("Paid GameCube").unwrap_or(html.len());
    let Some(t0) = html[..paid].find("<table") else { return Vec::new() };
    let table = &html[t0..html[t0..paid].find("</table>").map_or(paid, |e| t0 + e)];
    let mut packs = Vec::new();
    for row in table.split("<tr").skip(1) {
        let cells: Vec<&str> = row.split("<td").skip(1).map(|c| c.split_once('>').map_or("", |(_, r)| r)).collect();
        if cells.len() < 5 {
            continue;
        }
        let game = text_of(cells[0]);
        let size = parse_size(&text_of(cells[3])).unwrap_or(0);
        let author = text_of(cells[4]);
        let builds: Vec<Build> = cells[2]
            .split("<a ")
            .skip(1)
            .filter_map(|a| {
                let url = unescape(&attr(&format!(" {a}"), "href")?);
                let label = text_of(a.split_once('>')?.1.split("</a>").next()?);
                url.starts_with("http").then_some(Build { label, url, size })
            })
            .collect();
        if game.is_empty() || builds.is_empty() {
            continue;
        }
        let mut p = new_pack("gc", &game, &author, None, Some(PUTACHI.into()), builds, "putachi");
        p.region = Some(text_of(cells[1])).filter(|r| !r.is_empty() && r != "N/A");
        packs.push(p);
    }
    packs
}

fn text_of(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(out.split_whitespace().collect::<Vec<_>>().join(" ").trim())
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest[..rest.len().min(10)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "rsquo" | "lsquo" => Some('\''),
            "ndash" | "mdash" => Some('-'),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(c) = c {
            out.push(if matches!(c, '’' | '‘') { '\'' } else { c });
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Words of an author credit, for telling whether two lists credit the same people.
fn author_words(a: &str) -> Vec<String> {
    a.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !matches!(*w, "and" | "the" | "team" | "project" | "community"))
        .map(String::from)
        .collect()
}

fn same_game(a: &Pack, b: &Pack) -> bool {
    a.system == b.system
        && (norm(&a.game) == norm(&b.game)
            || matches!((&a.game_id, &b.game_id), (Some(x), Some(y)) if x.get(..3) == y.get(..3) && a.system == "gc"))
}

fn same_author(a: &Pack, b: &Pack) -> bool {
    let (x, y) = (author_words(&a.author), author_words(&b.author));
    x.iter().any(|w| y.contains(w)) || (!x.is_empty() && x == y)
}

/// One entry per pack: the same game by the same author from several lists becomes one
/// pack with every list's links (the first list's details win).
fn merge(packs: Vec<Pack>) -> Vec<Pack> {
    let mut out: Vec<Pack> = Vec::new();
    for p in packs {
        match out.iter_mut().find(|o| same_game(o, &p) && same_author(o, &p)) {
            Some(o) => {
                for b in p.builds {
                    if !o.builds.iter().any(|x| x.url == b.url) {
                        o.builds.push(b);
                    }
                }
                for s in p.sources {
                    if !o.sources.contains(&s) {
                        o.sources.push(s);
                    }
                }
                if o.game_id.is_none() {
                    o.game_id = p.game_id;
                }
                if o.page.is_none() {
                    o.page = p.page;
                }
                if o.summary.is_empty() {
                    o.summary = p.summary;
                }
                if o.about.is_empty() {
                    o.about = p.about;
                }
                o.status = o.status.take().or(p.status);
                o.region = o.region.take().or(p.region);
            }
            None => out.push(p),
        }
    }
    out.sort_by_cached_key(|p| (p.system.clone(), norm(&p.game), p.author.to_lowercase()));
    out
}

// ---------- the games on disk ----------

/// What texture folders are named after: a GameCube disc's 6-character ID, or an N64 ROM's
/// internal name. Reads only the file's header.
pub fn game_tag(system: &str, path: &Path) -> Option<String> {
    match system {
        "gc" => gc_id(path),
        "n64" => n64_name(path),
        _ => None,
    }
}

fn gc_id(path: &Path) -> Option<String> {
    let mut head = [0u8; 0x60];
    fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    // RVZ/WIA keep a copy of the disc header after their own 0x48-byte header
    let id = if head.starts_with(b"RVZ\x01") || head.starts_with(b"WIA\x01") { &head[0x58..0x5e] } else { &head[..6] };
    let id = std::str::from_utf8(id).ok()?;
    id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()).then(|| id.to_string())
}

fn n64_name(path: &Path) -> Option<String> {
    let mut head = [0u8; 0x40];
    if crate::library::has_ext(&path.to_string_lossy(), "zip") {
        let mut z = zip::ZipArchive::new(fs::File::open(path).ok()?).ok()?;
        let i = (0..z.len())
            .find(|&i| z.by_index(i).is_ok_and(|e| ["z64", "n64", "v64"].iter().any(|x| crate::library::has_ext(e.name(), x))))?;
        z.by_index(i).ok()?.read_exact(&mut head).ok()?;
    } else {
        fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    }
    rom_name(&mut head)
}

/// The internal name from an N64 ROM header in any of the three byte orders.
fn rom_name(head: &mut [u8; 0x40]) -> Option<String> {
    match head[..4] {
        [0x80, 0x37, 0x12, 0x40] => {}
        [0x37, 0x80, 0x40, 0x12] => head.as_chunks_mut::<2>().0.iter_mut().for_each(|c| c.swap(0, 1)),
        [0x40, 0x12, 0x37, 0x80] => head.as_chunks_mut::<4>().0.iter_mut().for_each(|c| c.reverse()),
        _ => return None,
    }
    let name: String = head[0x20..0x34].iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { ' ' }).collect();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Whether a pack is for this game (`tag` from `game_tag`, `name` the file name).
pub fn fits(p: &Pack, system: &str, name: &str, tag: Option<&str>) -> bool {
    if p.system != system {
        return false;
    }
    if let (Some(id), Some(tag)) = (&p.game_id, tag) {
        let hit = if system == "gc" { tag.starts_with(id.as_str()) } else { tag.eq_ignore_ascii_case(id) };
        if hit {
            return true;
        }
    }
    let (a, b) = (norm(&p.game), norm(name));
    // "zeldaocarinaoftime" fits "Legend of Zelda, The - Ocarina of Time"
    let slug = p.slug.as_deref().is_some_and(|s| s == b || (s.len() >= 10 && b.ends_with(s)));
    slug || (!a.is_empty() && a == b)
}

// ---------- where packs go ----------

#[derive(Debug, Clone)]
pub struct Dirs {
    /// RetroDECK's texture_packs folder.
    pub root: PathBuf,
    /// RetroArch's core options file (turns on N64 hi-res textures). None: leave alone.
    pub retroarch_opts: Option<PathBuf>,
}

impl Dirs {
    /// Next to the roms folder, as RetroDECK lays things out.
    pub fn for_roms(roms_dir: &Path) -> Self {
        let root = roms_dir.parent().unwrap_or(roms_dir).join("texture_packs");
        let retroarch_opts = directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".var/app/net.retrodeck.retrodeck/config/retroarch/retroarch-core-options.cfg"));
        Self { root, retroarch_opts }
    }

    /// Where the emulator looks for a pack's folder.
    pub fn live(&self, system: &str) -> PathBuf {
        match system {
            "n64" => self.root.join("retroarch-core/Mupen64Plus/hires_texture"),
            _ => self.root.join("Dolphin/Textures"),
        }
    }

    fn n64_cache(&self) -> PathBuf {
        self.root.join("retroarch-core/Mupen64Plus/cache")
    }

    fn store(&self, key: &str) -> PathBuf {
        self.root.join("romgetter").join(key.replace('/', "_"))
    }

    fn work(&self, key: &str) -> PathBuf {
        self.root.join("romgetter/.work").join(key.replace('/', "_"))
    }

    fn state_file(&self) -> PathBuf {
        self.root.join("romgetter/installed.json")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub system: String,
    /// Folder name in the emulator's texture folder (disc ID / ROM name).
    pub folder: String,
    pub build: String,
    pub enabled: bool,
    pub size: u64,
    /// N64 packs that come as GLideN64 cache files (.hts/.htc) instead of a folder.
    #[serde(default)]
    pub cache_files: Vec<String>,
}

/// Installed packs by key, kept next to them so it travels with the texture folder.
pub fn load_installed(d: &Dirs) -> HashMap<String, Installed> {
    fs::read(d.state_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_installed(d: &Dirs, all: &HashMap<String, Installed>) -> Result<()> {
    fs::create_dir_all(d.state_file().parent().unwrap_or(&d.root))?;
    fs::write(d.state_file(), serde_json::to_vec_pretty(all)?)?;
    Ok(())
}

/// Turn a pack on: move its folder into the emulator's. Any other pack for the same game
/// is turned off first. Refuses to replace a folder someone put there by hand.
pub fn enable(d: &Dirs, key: &str, all: &mut HashMap<String, Installed>) -> Result<()> {
    let inst = all.get(key).cloned().ok_or_else(|| anyhow!("not installed"))?;
    if inst.enabled {
        return Ok(());
    }
    let others: Vec<String> = all
        .iter()
        .filter(|(k, i)| *k != key && i.enabled && i.system == inst.system && i.folder == inst.folder)
        .map(|(k, _)| k.clone())
        .collect();
    for k in others {
        disable(d, &k, all)?;
    }
    let live = d.live(&inst.system);
    fs::create_dir_all(&live)?;
    let src = d.store(key).join(&inst.folder);
    let dest = live.join(&inst.folder);
    if src.exists() {
        if dest.exists() {
            bail!("{} already exists (put there by hand?); move it away first", dest.display());
        }
        fs::rename(&src, &dest).with_context(|| format!("moving the pack to {}", dest.display()))?;
    }
    let cache = d.n64_cache();
    for f in &inst.cache_files {
        fs::create_dir_all(&cache)?;
        fs::rename(d.store(key).join(f), cache.join(f)).with_context(|| format!("moving {f} to {}", cache.display()))?;
    }
    if inst.system == "n64"
        && let Some(opts) = &d.retroarch_opts
    {
        set_core_option(opts, "mupen64plus-txHiresEnable", "True")?;
    }
    all.get_mut(key).expect("checked above").enabled = true;
    save_installed(d, all)
}

/// Turn a pack off: move its folder back to romgetter's store.
pub fn disable(d: &Dirs, key: &str, all: &mut HashMap<String, Installed>) -> Result<()> {
    let inst = all.get(key).cloned().ok_or_else(|| anyhow!("not installed"))?;
    if !inst.enabled {
        return Ok(());
    }
    let store = d.store(key);
    fs::create_dir_all(&store)?;
    let live = d.live(&inst.system).join(&inst.folder);
    if live.exists() {
        fs::rename(&live, store.join(&inst.folder)).with_context(|| format!("moving {} back", live.display()))?;
    }
    for f in &inst.cache_files {
        let _ = fs::rename(d.n64_cache().join(f), store.join(f));
    }
    all.get_mut(key).expect("checked above").enabled = false;
    save_installed(d, all)
}

/// Delete a pack's files, wherever they are.
pub fn remove(d: &Dirs, key: &str, all: &mut HashMap<String, Installed>) -> Result<()> {
    if let Some(inst) = all.get(key).cloned() {
        if inst.enabled {
            let live = d.live(&inst.system).join(&inst.folder);
            if live.exists() {
                fs::remove_dir_all(&live).with_context(|| format!("deleting {}", live.display()))?;
            }
            for f in &inst.cache_files {
                let _ = fs::remove_file(d.n64_cache().join(f));
            }
        }
        all.remove(key);
    }
    let store = d.store(key);
    if store.exists() {
        fs::remove_dir_all(&store)?;
    }
    save_installed(d, all)
}

/// Set `key = "value"` in a RetroArch options file (adding the line if missing).
fn set_core_option(path: &Path, key: &str, value: &str) -> Result<()> {
    let Ok(text) = fs::read_to_string(path) else { return Ok(()) }; // no RetroArch here
    let line = format!("{key} = \"{value}\"");
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.split('=').next().is_some_and(|k| k.trim() == key) {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(line);
    }
    let new = out.join("\n") + "\n";
    if new != text {
        fs::write(path, new).with_context(|| format!("updating {}", path.display()))?;
    }
    Ok(())
}

// ---------- download + install ----------

#[derive(Debug, Clone)]
pub enum TexState {
    /// Waiting on a debrid service to fetch it.
    Remote {
        what: String,
        progress: f64,
    },
    Downloading {
        done: u64,
        total: u64,
    },
    Installing,
    Done(Installed),
    Failed(String),
}

/// Download one build of a pack and install it (turned on). `folder` names the emulator
/// folder after this game (its disc ID or ROM name), if it could be read.
pub async fn get(pack: Pack, build: Build, folder: Option<String>, d: Dirs, ctx: Ctx, tx: impl Fn(TexState) + Send + Sync + 'static) {
    let res = async {
        let work = d.work(&pack.key);
        if work.exists() {
            fs::remove_dir_all(&work)?;
        }
        fs::create_dir_all(&work)?;
        let files = fetch(&pack, &build, &work, &ctx, &tx).await?;
        tx(TexState::Installing);
        // a reinstall (another build, or a broken copy): the old one goes first
        if load_installed(&d).contains_key(&pack.key) {
            remove(&d, &pack.key, &mut load_installed(&d))?;
        }
        let (pack2, d2, build2) = (pack.clone(), d.clone(), build.label.clone());
        let mut inst = tokio::task::spawn_blocking(move || install(&files, &work, &pack2, folder.as_deref(), &d2, &build2)).await??;
        let mut all = load_installed(&d);
        all.insert(pack.key.clone(), inst.clone());
        save_installed(&d, &all)?;
        enable(&d, &pack.key, &mut all)?;
        inst.enabled = true;
        Ok::<_, anyhow::Error>(inst)
    }
    .await;
    tx(match res {
        Ok(i) => TexState::Done(i),
        Err(e) => TexState::Failed(format!("{e:#}")),
    });
}

/// Get the build's file(s) into `work`: file hosts through TorBox, then Real-Debrid;
/// plain file servers directly.
async fn fetch(pack: &Pack, build: &Build, work: &Path, ctx: &Ctx, tx: &impl Fn(TexState)) -> Result<Vec<PathBuf>> {
    let how = reach(&build.url);
    if how == Reach::Browser {
        bail!("this one is only on a web page; open it in a browser");
    }
    let progress = |s: LocalState| {
        if let LocalState::Downloading { done, total, .. } = s {
            tx(TexState::Downloading { done, total });
        }
    };
    let mut errors = Vec::new();
    // neither TorBox nor Real-Debrid takes plain file servers like EmulationKing's
    if how == Reach::Debrid
        && let Some(tb) = &ctx.torbox
    {
        match from_torbox(tb, pack, build, work, ctx, tx, &progress).await {
            Ok(files) => return Ok(files),
            Err(e) => {
                tx(TexState::Remote { what: format!("TorBox couldn't: {e:#}"), progress: 0.0 });
                errors.push(format!("TorBox: {e:#}"));
            }
        }
    }
    if how == Reach::Debrid
        && let Some(rd) = &ctx.rd
    {
        tx(TexState::Remote { what: "asking Real-Debrid".into(), progress: 0.0 });
        match rd.unrestrict(&build.url).await {
            Ok((url, name)) => {
                let dest = work.join(jobs::sanitize(&name));
                jobs::download(&ctx.http, &url, &dest, &progress).await?;
                return Ok(vec![dest]);
            }
            Err(e) => {
                tx(TexState::Remote { what: format!("Real-Debrid couldn't: {e:#}"), progress: 0.0 });
                errors.push(format!("Real-Debrid: {e:#}"));
            }
        }
    }
    if how == Reach::Direct {
        let name = build.url.rsplit('/').next().unwrap_or("pack.zip");
        let dest = work.join(jobs::sanitize(&urlencoding::decode(name).map_or_else(|_| name.to_string(), std::borrow::Cow::into_owned)));
        jobs::download(&ctx.http, &build.url, &dest, &progress).await?;
        return Ok(vec![dest]);
    }
    if errors.is_empty() {
        bail!("{} needs TorBox or Real-Debrid (or a browser)", build.label);
    }
    bail!("{}", errors.join("; "))
}

async fn from_torbox(
    tb: &crate::torbox::TorBox,
    pack: &Pack,
    build: &Build,
    work: &Path,
    ctx: &Ctx,
    tx: &impl Fn(TexState),
    progress: &impl Fn(LocalState),
) -> Result<Vec<PathBuf>> {
    tx(TexState::Remote { what: "sending to TorBox".into(), progress: 0.0 });
    let name = format!("{} - {} texture pack", pack.game, pack.author);
    let id = tb.create(Kind::WebDl, &build.url, Some(&name), false).await?;
    let started = std::time::Instant::now();
    let item = loop {
        let item = tb.get(Kind::WebDl, id).await?;
        if (item.download_present || item.download_finished) && !item.files.is_empty() {
            break item;
        }
        let state = item.download_state.to_lowercase();
        if state.contains("error") || state.contains("failed") {
            bail!("{}", item.download_state);
        }
        if started.elapsed() > Duration::from_hours(6) {
            bail!("still not done after 6 hours ({})", item.download_state);
        }
        tx(TexState::Remote { what: format!("TorBox: {}", item.download_state), progress: item.progress });
        tokio::time::sleep(ctx.poll).await;
    };
    let mut out = Vec::new();
    for f in &item.files {
        let dest = work.join(jobs::sanitize(&f.file_name()));
        let url = tb.request_dl(Kind::WebDl, id, f.id).await?;
        jobs::download(&ctx.http, &url, &dest, progress).await?;
        out.push(dest);
    }
    Ok(out)
}

/// Unpack the downloaded archive(s), find the folder of textures, and put it in the store
/// under the name the emulator wants. Blocking.
fn install(files: &[PathBuf], work: &Path, pack: &Pack, folder: Option<&str>, d: &Dirs, build: &str) -> Result<Installed> {
    let x = work.join("x");
    fs::create_dir_all(&x)?;
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if later_part(&name) {
            continue; // the first part's extraction reads the rest
        }
        if ["zip", "7z", "rar"].iter().any(|e| crate::library::has_ext(&name, e)) {
            unpack(f, &x).with_context(|| format!("unpacking {name}"))?;
        } else {
            fs::rename(f, x.join(f.file_name().unwrap_or_default()))?;
        }
    }
    let store = d.store(&pack.key);
    if store.exists() {
        fs::remove_dir_all(&store)?;
    }
    fs::create_dir_all(&store)?;
    // N64 packs sometimes ship as ready-made GLideN64 caches
    let mut cache_files = Vec::new();
    if pack.system == "n64" {
        for f in walk(&x).into_iter().filter(|p| ["hts", "htc"].iter().any(|e| crate::library::has_ext(&p.to_string_lossy(), e))) {
            let n = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
            fs::rename(&f, store.join(&n))?;
            cache_files.push(n);
        }
    }
    let root = pack_root(&x, &pack.system, folder);
    let images = walk(&root).iter().filter(|p| is_texture(p)).count();
    if images == 0 && cache_files.is_empty() {
        bail!("no textures in the download");
    }
    let found_name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = match (pack.system.as_str(), folder) {
        // Dolphin: keep the pack's own ID folder if it's for this disc ("GLM" covers GLME01)
        ("gc", Some(id)) if is_disc_id(&found_name) && id.starts_with(found_name.as_str()) => found_name,
        (_, Some(f)) => f.to_string(),
        _ if root != x && !found_name.is_empty() => found_name,
        _ => pack.game_id.clone().unwrap_or_else(|| jobs::sanitize(&pack.game)),
    };
    if images > 0 {
        fs::rename(&root, store.join(&folder)).context("moving the textures into place")?;
    }
    let size = walk(&store).iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
    let _ = fs::remove_dir_all(work);
    Ok(Installed { system: pack.system.clone(), folder, build: build.into(), enabled: false, size, cache_files })
}

/// "x.part2.rar", "x.r00", "x.7z.002", "x.z01": not the part to open.
fn later_part(name: &str) -> bool {
    let n = name.to_lowercase();
    if let Some(stem) = n.strip_suffix(".rar")
        && let Some((_, num)) = stem.rsplit_once(".part")
    {
        return num.parse::<u32>().is_ok_and(|n| n > 1);
    }
    let ext = n.rsplit('.').next().unwrap_or("");
    let numbered = |p: &str| ext.strip_prefix(p).is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
    numbered("r") || numbered("z") || (ext.len() == 3 && ext.chars().all(|c| c.is_ascii_digit()) && ext != "001")
}

fn is_texture(p: &Path) -> bool {
    ["png", "dds"].iter().any(|e| crate::library::has_ext(&p.to_string_lossy(), e))
}

/// The folder holding the pack: a folder named for this game if there is one, else the
/// top of the archive with wrapper folders peeled off ("Pack v2/GLM/…" → "GLM").
fn pack_root(x: &Path, system: &str, folder: Option<&str>) -> PathBuf {
    let dirs: Vec<PathBuf> = walk_dirs(x, 4);
    let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(f) = folder {
        let hit = dirs.iter().find(|p| {
            let n = name_of(p);
            if system == "gc" { is_disc_id(&n) && f.starts_with(n.as_str()) } else { n.eq_ignore_ascii_case(f) }
        });
        if let Some(h) = hit {
            return h.clone();
        }
    }
    if system == "gc"
        && let Some(h) = dirs.iter().find(|p| is_disc_id(&name_of(p)))
    {
        return h.clone();
    }
    let mut cur = x.to_path_buf();
    loop {
        let Ok(rd) = fs::read_dir(&cur) else { return cur };
        let entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        let subdirs: Vec<&PathBuf> = entries.iter().filter(|p| p.is_dir()).collect();
        let images_here = entries.iter().any(|p| is_texture(p));
        if images_here || subdirs.len() != 1 {
            return cur;
        }
        cur.clone_from(subdirs[0]);
    }
}

fn walk_dirs(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut level = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for d in level {
            for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    next.push(e.path());
                }
            }
        }
        next.sort();
        out.extend(next.iter().cloned());
        level = next;
    }
    out
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => out.push(e.path()),
                _ => {}
            }
        }
    }
    out
}

/// Unpack a zip or 7z in-process; rar (and anything those fail on) with the system's
/// 7z, unrar or bsdtar, which SteamOS has.
fn unpack(src: &Path, dest: &Path) -> Result<()> {
    let name = src.to_string_lossy().to_lowercase();
    let native = if crate::library::has_ext(&name, "zip") {
        fs::File::open(src).map_err(anyhow::Error::from).and_then(|f| Ok(zip::ZipArchive::new(f)?.extract(dest)?))
    } else if crate::library::has_ext(&name, "7z") {
        sevenz_rust2::decompress_file(src, dest).map_err(|e| anyhow!("{e}"))
    } else {
        Err(anyhow!("not built in"))
    };
    let Err(first) = native else { return Ok(()) };
    let (s, d) = (src.to_string_lossy().into_owned(), dest.to_string_lossy().into_owned());
    let tools: [(&str, Vec<String>); 3] = [
        ("7z", vec!["x".into(), "-y".into(), format!("-o{d}"), s.clone()]),
        ("unrar", vec!["x".into(), "-o+".into(), "-idq".into(), s.clone(), format!("{d}/")]),
        ("bsdtar", vec!["-xf".into(), s, "-C".into(), d]),
    ];
    for (tool, args) in tools {
        match std::process::Command::new(tool).args(&args).stdin(std::process::Stdio::null()).output() {
            Ok(o) if o.status.success() => return Ok(()),
            Ok(_) | Err(_) => {}
        }
    }
    bail!("{first:#}; no 7z/unrar/bsdtar could open it either")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_emulationking() {
        let md = "## Pages\n\n- [coolhardy100's Luigi's Mansion Texture Pack](https://emulationking.com/nintendo/gamecube/games/luigismansion/texturepacks/coolhardy100/index.md): Around 3,500 textures.\n- [Kazangwa’s CBFD Retexture](https://emulationking.com/nintendo/n64/games/conkersbadfurday/texturepacks/kazangwa/index.md): x\n- [Home](https://emulationking.com/)";
        let l = parse_ek_list(md);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].1, "https://emulationking.com/nintendo/gamecube/games/luigismansion/texturepacks/coolhardy100/");
        assert_eq!(split_title(&l[0].0), ("coolhardy100".into(), "Luigi's Mansion".into()));
        assert_eq!(l[0].2, "Around 3,500 textures.");
        assert_eq!(split_title(&l[1].0), ("Kazangwa".into(), "CBFD".into()));

        let html = r#"<li><span><strong>Bluedart, <a href=/authors/djipi/>Djipi</a></strong>Artist</span></li><li><svg></svg>
<span><strong>Complete</strong>Status</span></li>
<div class=console-overview-text><p>Bluedart&rsquo;s pack is complete.</p><p>It mixes
 several artists.</p></div><p>go into <code>Load/Textures/</code>, so you end up with a <code>Load/Textures/GLM/</code> folder</p>
<h2 id=download>Download</h2><div class=dl-tiles><a class="dl-tile dl-tile-download" href=https://files.emulationking.com/gamecube/texturepacks/luigismansion/coolhardy100/LM-DDS.7z rel="nofollow" title="Download coolhardy100&#39;s Luigi&#39;s Mansion Texture Pack  (DDS, 608 MB)" data-x=1><span>DDS</span></a><a class="dl-tile dl-tile-download" href=https://files.emulationking.com/x/LM-PNG.7z title="Download it  (PNG, 1.06 GB)">x</a></div><p class=dl-feed>"#;
        let EkPage { builds: b, id, about, status } = parse_ek_page(html);
        assert_eq!(about, ["Bluedart's pack is complete.", "It mixes several artists."]);
        assert_eq!(status.as_deref(), Some("Complete"));
        assert_eq!(id.as_deref(), Some("GLM"));
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].label, "DDS");
        assert_eq!(b[0].size, 608 << 20);
        assert!(b[0].url.ends_with("LM-DDS.7z"));
        assert_eq!(b[1].label, "PNG");
        let rice = r#"id=download <a class="dl-tile" href=https://files.emulationking.com/n64/texturepacks/cbfd/ConkerBFD-Kazangwa.zip title="Download Kazangwa’s CBFD Retexture  (Rice Format)">"#;
        assert_eq!(parse_ek_page(rice).builds[0].label, "Rice Format");
    }

    #[test]
    fn parses_rapka_and_putachi() {
        let md = "Installation: …\n\n[GM8E01] Metroid Prime: https://mega.nz/#!2JED!9UVh\n\n[NAKE01] Pokemon Snap: https://mega.nz/#!OZ1R!l225\n";
        let r = parse_rapka(md);
        assert_eq!(r.len(), 1, "only GameCube discs");
        assert_eq!((r[0].game.as_str(), r[0].game_id.as_deref()), ("Metroid Prime", Some("GM8E01")));

        let html = r#"<h2>GameCube Texture Packs (Free)</h2><table><tr><th>GAME TITLE</th></tr>
<tr><td>Luigi&#8217;s Mansion (4K)</td><td>N/A</td><td><a href="https://drive.google.com/file/d/1Fz/view">Drive</a></td><td>1.1 GB</td><td>coolhardy100</td></tr>
<tr><td>Mario Kart Double Dash Re-Rendered!!</td><td>NTSC-U</td><td><a href="https://app.filen.io/a">Filen (PNG)</a> &amp; <a href="https://app.filen.io/b">Filen (DDS)</a></td><td>803 MB</td><td>Giodude</td></tr>
</table><h2>Paid GameCube Texture Packs</h2><table><tr><td>Paid</td><td>x</td><td><a href="https://patreon.com/x">P</a></td><td>1 GB</td><td>y</td></tr></table>"#;
        let p = parse_putachi(html);
        assert_eq!(p.len(), 2, "free table only");
        assert_eq!(p[0].region, None, "N/A");
        assert_eq!(p[1].region.as_deref(), Some("NTSC-U"));
        assert_eq!(p[0].game, "Luigi's Mansion (4K)");
        assert_eq!(p[0].builds[0].size, (1.1 * f64::from(1 << 30)) as u64);
        assert_eq!(p[1].builds.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), ["Filen (PNG)", "Filen (DDS)"]);
        assert_eq!(reach(&p[0].builds[0].url), Reach::Debrid);
        assert_eq!(reach(&p[1].builds[0].url), Reach::Browser);
    }

    #[test]
    fn merges_the_same_pack_from_several_lists() {
        let b = |u: &str| vec![Build { label: "x".into(), url: u.into(), size: 0 }];
        let packs = vec![
            new_pack(
                "gc",
                "Luigi's Mansion",
                "coolhardy100",
                Some("GLM".into()),
                Some("ek".into()),
                b("https://files.emulationking.com/a.7z"),
                "EmulationKing",
            ),
            new_pack("gc", "Luigi's Mansion", "qashto", None, None, b("https://files.emulationking.com/q.7z"), "EmulationKing"),
            new_pack("gc", "Luigi's Mansion (4K)", "coolhardy100", None, None, b("https://drive.google.com/x"), "putachi"),
            new_pack(
                "gc",
                "Skies of Arcadia Legends",
                "Bighead and frozenwings",
                None,
                None,
                b("https://files.emulationking.com/s.zip"),
                "EmulationKing",
            ),
            new_pack(
                "gc",
                "Skies Of Arcadia Legends",
                "1vierock, Bighead & Frozenwings",
                None,
                None,
                b("https://gumroad.com/s"),
                "putachi",
            ),
            new_pack("gc", "Metroid Prime", "rapka", Some("GM8E01".into()), None, b("https://mega.nz/m"), "rapka"),
        ];
        let m = merge(packs);
        assert_eq!(m.len(), 4);
        let lm = m.iter().find(|p| p.author == "coolhardy100").unwrap();
        assert_eq!(lm.builds.len(), 2);
        assert_eq!(lm.sources, ["EmulationKing", "putachi"]);
        assert_eq!(lm.page.as_deref(), Some("ek"));
        assert_eq!(m.iter().find(|p| p.game.starts_with("Skies")).unwrap().builds.len(), 2);
    }

    #[test]
    fn fits_games_by_id_or_title() {
        let p = new_pack("gc", "Luigi's Mansion", "x", Some("GLM".into()), None, vec![], "EmulationKing");
        assert!(fits(&p, "gc", "Some Other Name (USA).rvz", Some("GLME01")));
        assert!(fits(&p, "gc", "Luigi's Mansion (USA).rvz", None));
        assert!(!fits(&p, "gc", "Mario Party 4 (USA).rvz", Some("GMPE01")));
        assert!(!fits(&p, "n64", "Luigi's Mansion (USA).z64", None));
        let z = new_pack("gc", "The Legend of Zelda: The Wind Waker", "x", None, None, vec![], "putachi");
        assert!(fits(&z, "gc", "Legend of Zelda, The - The Wind Waker (USA).rvz", Some("GZLE01")));
        let c = new_pack("n64", "CBFD", "Kazangwa", Some("CONKER BFD".into()), None, vec![], "EmulationKing");
        assert!(fits(&c, "n64", "Conker's Bad Fur Day (USA).zip", Some("CONKER BFD")));
        let mut oot = new_pack("n64", "Djipis 2009 Cellshade Ocarina of Time", "", None, None, vec![], "EmulationKing");
        assert!(!fits(&oot, "n64", "Legend of Zelda, The - Ocarina of Time (USA).zip", Some("THE LEGEND OF ZELDA")));
        oot.slug = Some("zeldaocarinaoftime".into());
        assert!(fits(&oot, "n64", "Legend of Zelda, The - Ocarina of Time (USA).zip", Some("THE LEGEND OF ZELDA")));
        assert!(!fits(&oot, "n64", "Legend of Zelda, The - Majora's Mask (USA).zip", None));
        let mut bk = new_pack("n64", "hi-res Banjo-Kazooie!", "Macphisto", None, None, vec![], "EmulationKing");
        bk.slug = Some("banjokazooie".into());
        assert!(fits(&bk, "n64", "Banjo-Kazooie (USA).zip", None));
        assert!(!fits(&bk, "n64", "Banjo-Tooie (USA).zip", None));
    }

    #[test]
    fn reads_game_tags() {
        let dir = std::env::temp_dir().join(format!("romgetter-tex-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut iso = vec![0u8; 0x100];
        iso[..6].copy_from_slice(b"GMBE8P");
        fs::write(dir.join("a.iso"), &iso).unwrap();
        let mut rvz = vec![0u8; 0x100];
        rvz[..4].copy_from_slice(b"RVZ\x01");
        rvz[0x58..0x5e].copy_from_slice(b"GMSE01");
        fs::write(dir.join("b.rvz"), &rvz).unwrap();
        assert_eq!(game_tag("gc", &dir.join("a.iso")).as_deref(), Some("GMBE8P"));
        assert_eq!(game_tag("gc", &dir.join("b.rvz")).as_deref(), Some("GMSE01"));

        let mut rom = [0u8; 0x40];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        rom[0x20..0x34].copy_from_slice(b"CONKER BFD          ");
        let mut v64 = rom;
        v64.as_chunks_mut::<2>().0.iter_mut().for_each(|c| c.swap(0, 1));
        let mut n64 = rom;
        n64.as_chunks_mut::<4>().0.iter_mut().for_each(|c| c.reverse());
        for mut h in [rom, v64, n64] {
            assert_eq!(rom_name(&mut h).as_deref(), Some("CONKER BFD"));
        }
        // inside a zip, as RetroDECK keeps N64 games
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("Conker.z64", zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut z, &rom).unwrap();
            z.finish().unwrap();
        }
        fs::write(dir.join("c.zip"), buf.into_inner()).unwrap();
        assert_eq!(game_tag("n64", &dir.join("c.zip")).as_deref(), Some("CONKER BFD"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn installs_enables_and_disables() {
        let root = std::env::temp_dir().join(format!("romgetter-texinst-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let opts = root.join("retroarch-core-options.cfg");
        let d = Dirs { root: root.join("texture_packs"), retroarch_opts: Some(opts.clone()) };
        fs::create_dir_all(&root).unwrap();
        fs::write(&opts, "mupen64plus-txFilterMode = \"None\"\nmupen64plus-txHiresEnable = \"False\"\n").unwrap();

        // GameCube: a wrapper folder around the ID folder
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("Pack v2/readme.txt", o).unwrap();
            z.start_file("Pack v2/GLM/tex1_64x64_abc.png", o).unwrap();
            std::io::Write::write_all(&mut z, b"png").unwrap();
            z.finish().unwrap();
        }
        let p = new_pack("gc", "Luigi's Mansion", "coolhardy100", Some("GLM".into()), None, vec![], "EmulationKing");
        let work = d.work(&p.key);
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("lm.zip"), buf.into_inner()).unwrap();
        let inst = install(&[work.join("lm.zip")], &work, &p, Some("GLME01"), &d, "DDS").unwrap();
        assert_eq!(inst.folder, "GLM");
        let mut all = HashMap::from([(p.key.clone(), inst)]);
        enable(&d, &p.key, &mut all).unwrap();
        assert!(d.live("gc").join("GLM/tex1_64x64_abc.png").exists());
        disable(&d, &p.key, &mut all).unwrap();
        assert!(!d.live("gc").join("GLM").exists());
        assert!(d.store(&p.key).join("GLM/tex1_64x64_abc.png").exists());
        assert_eq!(load_installed(&d).get(&p.key).map(|i| i.enabled), Some(false));

        // a folder put there by hand is left alone
        fs::create_dir_all(d.live("gc").join("GLM")).unwrap();
        assert!(enable(&d, &p.key, &mut all).is_err());
        fs::remove_dir_all(d.live("gc").join("GLM")).unwrap();

        // N64: the folder is renamed to the ROM's name, and hi-res textures get turned on
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("Author.txt", o).unwrap();
            z.start_file("Conker/CONKER BFD#0070EAE3#2#0#97F4EF32_ciByRGBA.png", o).unwrap();
            z.finish().unwrap();
        }
        let c = new_pack("n64", "Conker's Bad Fur Day", "Kazangwa", None, None, vec![], "EmulationKing");
        let work = d.work(&c.key);
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("c.zip"), buf.into_inner()).unwrap();
        let inst = install(&[work.join("c.zip")], &work, &c, Some("CONKER BFD"), &d, "Rice Format").unwrap();
        assert_eq!(inst.folder, "CONKER BFD");
        all.insert(c.key.clone(), inst);
        enable(&d, &c.key, &mut all).unwrap();
        assert!(d.live("n64").join("CONKER BFD").is_dir());
        let cfg = fs::read_to_string(&opts).unwrap();
        assert!(cfg.contains("mupen64plus-txHiresEnable = \"True\"") && cfg.contains("txFilterMode"), "{cfg}");

        remove(&d, &c.key, &mut all).unwrap();
        assert!(!d.live("n64").join("CONKER BFD").exists());
        assert!(!all.contains_key(&c.key));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn default_builds() {
        let b = |l: &str| Build { label: l.into(), url: format!("https://files.emulationking.com/{l}.zip"), size: 0 };
        let mut n = new_pack("n64", "x", "y", None, None, vec![b("Rice Format"), b("Glide64 Cache"), b("GlideN64 Cache")], "EmulationKing");
        assert_eq!(n.default_build(), Some(2));
        n.builds.truncate(2);
        assert_eq!(n.default_build(), Some(0));
        let g = new_pack("gc", "x", "y", None, None, vec![b("Native"), b("Lite")], "EmulationKing");
        assert_eq!(g.default_build(), Some(1));
        let web = new_pack(
            "gc",
            "x",
            "y",
            None,
            None,
            vec![Build { label: "P".into(), url: "https://patreon.com/x".into(), size: 0 }],
            "putachi",
        );
        assert_eq!(web.default_build(), None);
    }

    #[test]
    fn picks_archive_parts() {
        assert!(later_part("x.part2.rar"));
        assert!(!later_part("x.part1.rar"));
        assert!(!later_part("x.rar"));
        assert!(later_part("x.r00"));
        assert!(later_part("x.7z.002"));
        assert!(!later_part("x.7z.001"));
        assert!(!later_part("x.zip"));
        assert!(later_part("x.z01"));
    }

    #[tokio::test]
    #[ignore = "network; a report, run with --nocapture"]
    async fn live_catalog() {
        let c = fetch_catalog(&reqwest::Client::new()).await.unwrap();
        let by = |s: &str| c.packs.iter().filter(|p| p.sources.iter().any(|x| x == s)).count();
        println!("{} packs · EmulationKing {} · rapka {} · putachi {}", c.packs.len(), by("EmulationKing"), by("rapka"), by("putachi"));
        for p in c.packs.iter().filter(|p| p.sources.len() > 1) {
            println!("  merged: {} / {} ({:?})", p.game, p.author, p.sources);
        }
        let n = |f: &dyn Fn(&Pack) -> bool| c.packs.iter().filter(|p| f(p)).count();
        println!(
            "summary {} · about {} · status {} · region {}",
            n(&|p| !p.summary.is_empty()),
            n(&|p| !p.about.is_empty()),
            n(&|p| p.status.is_some()),
            n(&|p| p.region.is_some())
        );
        for p in c.packs.iter().filter(|p| p.game.contains("Majora")).take(2) {
            println!("  {} / {}: {:?} {:?}\n    {}\n    {:?}", p.game, p.author, p.status, p.region, p.summary, p.about);
        }
        assert!(by("EmulationKing") > 50);
        assert!(n(&|p| !p.about.is_empty()) > 50, "overviews parsed");
    }
}
