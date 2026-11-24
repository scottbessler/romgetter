use crate::archive::{self, ItemHit, ItemIndex};
use crate::config::{Config, LIVE_ARCHIVE, LIVE_VIMM};
use crate::gamepad::{self, Pad};
use crate::ifarchive;
use crate::jobs::{self, LocalState, LocalTx};
use crate::library::{self, Game, Library, SourceKind};
use crate::minerva;
use crate::sources;
use crate::systems;
use crate::tbzips;
use crate::torbox::{Item, Kind};
use crate::vimm;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc::UnboundedSender};

mod settings;
mod tex;
pub use settings::{CfgAct, CfgRow, CfgTab, Field, tilde};
pub use tex::{TexRow, TexTab};

/// Re-index cached sets older than this (in the background; the cache stays usable).
const REINDEX_AFTER: Duration = Duration::from_hours(336);
/// Wait this long after the last keystroke before running the (slow) live search.
const LIVE_DEBOUNCE: Duration = Duration::from_millis(700);
/// Back off this long before automatically re-sending a game TorBox rejected.
const RETRY_AFTER: Duration = Duration::from_mins(5);
/// How often to re-check a game TorBox only has part of the torrent for.
const PARTIAL_RETRY: Duration = Duration::from_hours(6);
const MAX_SENDING: usize = 3;
/// Library downloads running at once when downloading in bulk (D); the rest wait.
const MAX_DOWNLOADS: usize = 2;
/// How many of Metacritic's best games `t` adds for a system.
const TOP_N: usize = 20;
const MAX_LOCAL_HITS: usize = 2000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Search,
    Library,
    Textures,
    Sources,
    Config,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Search, Tab::Library, Tab::Textures, Tab::Sources, Tab::Config];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Search => "1 Search",
            Tab::Library => "2 Library",
            Tab::Textures => "3 Textures",
            Tab::Sources => "4 Sources",
            Tab::Config => "5 Config",
        }
    }
}

/// Where search sets come from, in the Sources tab's order.
pub const SET_GROUPS: &[&str] = &[
    "Minerva · No-Intro",
    "Minerva · Redump",
    "Your TorBox torrents",
    "archive.org · built-in sets",
    "archive.org · restricted (log in on the Config tab)",
    "archive.org · your extra sets",
    "IF Archive",
    LIVE_GROUP,
];

/// The Sources tab's group of sites searched as you type.
pub const LIVE_GROUP: &str = "Live search (as you type)";
/// Its rows: (config key, title).
pub const LIVE_SOURCES: &[(&str, &str)] =
    &[(LIVE_ARCHIVE, "archive.org search"), (LIVE_VIMM, "Vimm's Lair (downloads open in a browser; a last resort)")];

/// A row of the Sources tab.
pub enum SetRow {
    Group {
        name: &'static str,
        sets: usize,
        /// Of those, switched off.
        off: usize,
        files: usize,
        collapsed: bool,
    },
    /// A search set (its `SetRef::item`).
    Set(String),
    /// A live-search site (its `LIVE_SOURCES` key).
    Live(&'static str),
}

/// What the Sources tab shows for one set.
pub enum SetState {
    Indexed,
    Indexing,
    /// Not indexed yet (waiting for a slot, or nothing tried).
    Waiting,
    Failed(String),
    /// Restricted on archive.org and not logged in: hidden from search.
    Locked,
}

pub struct SetInfo {
    pub title: String,
    pub system: String,
    pub files: usize,
    pub size: u64,
    pub state: SetState,
    /// Cached on TorBox (a torrent in the account): games come straight away.
    pub cached: bool,
    /// Switched on in the Sources tab (searched).
    pub on: bool,
    /// Debrid services caching the set's torrent: ("TorBox", state, progress, done).
    pub caching: Vec<(&'static str, String, f64, bool)>,
    /// Indexed this many seconds ago.
    pub age: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibInput {
    Filter,
    Paste,
}

/// Library order within each system (`o` switches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibSort {
    /// systems in SYSTEMS order; games needing attention first, then by name
    #[default]
    Name,
    /// biggest systems first, biggest games first
    Size,
}

/// What TorBox knows about a library game.
#[derive(Debug, Clone)]
pub enum Remote {
    NoKey,
    /// Downloaded straight from its source, never through TorBox (IF Archive files).
    Direct,
    NotSent,
    Sending,
    Pending {
        state: String,
        progress: f64,
    },
    Ready,
    /// Had a TorBox id but it's no longer in the account (expired/deleted).
    Missing,
    Error(String),
}

/// A set Search indexes: built-in or from `extra_sets` in the config.
#[derive(Clone)]
pub struct SetRef {
    pub item: String,
    /// empty = detect
    pub system: String,
}

#[derive(Clone)]
pub struct RomHit {
    pub url: String,
    pub name: String,
    pub size: u64,
    pub system: String,
    /// Where it came from (set title or archive.org item).
    pub source: String,
    pub live: bool,
    /// (info-hash, index in torrent) for files that live inside a torrent (Minerva).
    pub torrent: Option<(String, Option<u32>)>,
    /// Full title for MAME short-name zips.
    pub title: Option<String>,
}

/// Torrents TorBox is still downloading (see `tbzips::pending`).
type TorboxPending = HashMap<String, (String, f64)>;

/// What each torrent in the TorBox account holds (see `tbzips::held`).
pub type TorboxHeld = HashMap<String, Option<HashSet<String>>>;

/// A row of search results: a system header, or one game with every source found for it.
pub enum SearchRow {
    Header {
        system: String,
        count: usize,
        collapsed: bool,
    },
    /// Indices into `rom_hits`, best source first.
    Game(Vec<usize>),
}

/// A file name parsed for grouping search results (cached: parsing is most of a keystroke).
struct NameInfo {
    /// Same game, same key (see `names::GameName::key`).
    key: String,
    display: String,
    preference: (u8, u8, std::cmp::Reverse<(u32, String)>),
}

/// One set's files prepared for instant search (built once, not per keystroke).
pub struct SetHay {
    /// "title item", lowercase
    set_text: String,
    /// per file: lowercase name (+ MAME title), its system, the system's lowercase label, its MAME title
    files: Vec<(String, &'static str, &'static str, Option<String>)>,
}

pub enum Row {
    Header {
        system: String,
        count: usize,
        collapsed: bool,
    },
    Game(usize),
    /// "… N more": the group is capped (see LIB_GROUP_LIMIT)
    More {
        system: String,
        hidden: usize,
    },
}

/// A popup list of actions (Enter on an item, or `m` / Y for the tab).
pub struct Menu {
    pub title: String,
    pub items: Vec<(String, Act)>,
    pub sel: usize,
}

/// A debrid service a set can be cached to from the Sources tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Debrid {
    TorBox,
    RealDebrid,
}

impl Debrid {
    pub fn name(self) -> &'static str {
        match self {
            Debrid::TorBox => "TorBox",
            Debrid::RealDebrid => "Real-Debrid",
        }
    }
}

/// A set torrent sent to a debrid service to cache whole: its state there.
#[derive(Clone, Debug)]
pub struct CacheJob {
    pub state: String,
    pub progress: f64,
    pub done: bool,
}

/// Sets this big or bigger aren't offered for caching (the services balk, and it's slow).
const CACHE_MAX: u64 = 1_000_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    AddRom,
    /// Search: add the game from this hit (index into `rom_hits`).
    AddSource(usize),
    /// Search: list the selected game's sources.
    Sources,
    Download,
    DownloadDirect,
    /// "System: x ◂ ▸", changed in place with ←/→ (or Enter).
    System,
    Remove,
    DeleteAsk,
    DeleteYes,
    Fold,
    TopGames,
    ShowAll,
    Search,
    DownloadAll,
    Paste,
    Sort,
    CollapseAll,
    ExpandAll,
    Rescan,
    /// Sources tab: switch the row's source(s) on or off.
    ToggleSource,
    /// Sources tab: have this debrid service cache the whole set torrent.
    CacheTo(Debrid),
    /// Config tab: do this to the selected setting.
    Cfg(CfgAct),
    /// Textures tab: install build N of the selected pack.
    TexInstall(usize),
    /// Open build N's page (hosts only a person can use).
    TexOpenUrl(usize),
    TexToggle,
    TexRemove,
    TexPage,
    TexRefresh,
    /// Sources tab: search the selected set's system / index it again.
    SetSearch,
    Reindex,
    Quit,
    Close,
}

fn system_label(sys: &str) -> String {
    let name = if sys.is_empty() { "? (unknown)" } else { systems::label(sys) };
    format!("System: ◂ {name} ▸")
}

/// What the Library cursor is on; stable across re-sorting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    Header(String),
    Game(String),
    More(String),
}

/// Games shown per system before "… N more" (unless the list is being filtered).
pub const LIB_GROUP_LIMIT: usize = 20;
/// Libraries bigger than this open with every system collapsed.
const AUTO_COLLAPSE_OVER: usize = 40;

pub enum Msg {
    MameTitles(anyhow::Result<HashMap<String, String>>),
    Indexed(String, anyhow::Result<ItemIndex>),
    MinervaFolders(String, anyhow::Result<Vec<minerva::Folder>>),
    /// Zipped ROM-set torrents found in the TorBox account.
    TorboxZips(anyhow::Result<(Vec<tbzips::Candidate>, TorboxHeld, TorboxPending)>),
    /// A set torrent sent to a debrid service to cache (service, info-hash, result).
    CacheSent(Debrid, String, anyhow::Result<String>),
    /// TorBox's cached files per Minerva torrent (hashes asked about, result).
    MinervaCache(Vec<String>, anyhow::Result<HashMap<String, HashSet<String>>>),
    /// A game was put on Real-Debrid (game id, RD torrent id).
    RdSent(String, anyhow::Result<String>),
    /// Real-Debrid's torrent list (status of every game on it).
    RdList(anyhow::Result<Vec<crate::realdebrid::ListItem>>),
    /// Metacritic's best titles for a system.
    TopGames(String, anyhow::Result<Vec<String>>),
    Live(u64, anyhow::Result<Vec<(ItemHit, ItemIndex)>>),
    /// Vimm's Lair search results (same generation counter as Live).
    Vimm(u64, anyhow::Result<Vec<vimm::Hit>>),
    Sent(String, anyhow::Result<u64>),
    Remote(Kind, anyhow::Result<Vec<Item>>),
    Local(String, LocalState),
    Scanned(Vec<library::Found>),
    /// The texture pack lists were fetched.
    TexCatalog(anyhow::Result<crate::textures::Catalog>),
    /// A texture pack's download/install moved along (pack key, state).
    Tex(String, crate::textures::TexState),
    /// A set's search text, built in the background (set, the index it's from, MAME titles generation).
    Hay(String, Arc<ItemIndex>, u64, SetHay),
    /// The search-text worker finished its batch.
    HaysDone,
    /// Config tab: who an API key belongs to, or why the service refused it.
    KeyChecked(Field, anyhow::Result<String>),
    /// Config tab: archive.org login came back.
    ArchiveLogin(anyhow::Result<crate::config::ArchiveLogin>),
}

const MINERVA_REFRESH: Duration = Duration::from_hours(168);

fn minerva_sets(group: &str, folders: &[minerva::Folder]) -> Vec<SetRef> {
    folders
        .iter()
        .filter_map(|f| {
            minerva::folder_system(&f.name).map(|sys| SetRef { item: minerva::set_key(group, &f.name), system: sys.to_string() })
        })
        .collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FolderCache {
    fetched: u64,
    folders: Vec<minerva::Folder>,
}

fn folders_path(cache_dir: &Path, group: &str) -> PathBuf {
    cache_dir.join(format!("minerva-{group}-folders.json"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// (folders, still fresh?)
fn load_folders(cache_dir: &Path, group: &str) -> Option<(Vec<minerva::Folder>, bool)> {
    let c: FolderCache = serde_json::from_str(&std::fs::read_to_string(folders_path(cache_dir, group)).ok()?).ok()?;
    let fresh = now_secs().saturating_sub(c.fetched) < MINERVA_REFRESH.as_secs();
    Some((c.folders, fresh))
}

fn save_folders(cache_dir: &Path, group: &str, folders: &[minerva::Folder]) {
    let c = FolderCache { fetched: now_secs(), folders: folders.to_vec() };
    let _ = std::fs::create_dir_all(cache_dir);
    if let Ok(json) = serde_json::to_vec(&c) {
        let _ = std::fs::write(folders_path(cache_dir, group), json);
    }
}

/// A set file's system: its own (sets of several systems), its extension or folders,
/// then the set's.
fn file_system<'a>(f: &'a archive::RomFile, set_system: &'a str) -> &'a str {
    f.system.as_deref().or_else(|| systems::guess_for_file(&f.name)).unwrap_or(set_system)
}

fn tb_zips_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("torbox-zips.json")
}

/// Search sets for zipped TorBox torrents; they go right after Minerva's.
fn tb_zip_sets(cands: &[tbzips::Candidate]) -> Vec<SetRef> {
    cands.iter().map(|c| SetRef { item: c.key(), system: c.system.clone() }).collect()
}

/// Where new TorBox-zip sets go in the search order: after Minerva's sets.
fn tb_zip_pos(sets: &[SetRef]) -> usize {
    sets.iter().take_while(|s| s.item.starts_with("minerva:")).count()
}

/// Minerva folders up to this size may be fetched uncached: TorBox downloads the whole
/// folder torrent. Bigger ones (the disc systems run to tens of TB) only work for files
/// TorBox already has cached.
const UNCACHED_MAX: u64 = 25_000_000_000;

/// How long TorBox's cache status for Minerva torrents is trusted before re-checking.
const CACHE_STATUS_REFRESH: Duration = Duration::from_hours(24);

/// Which files of a Minerva set search shows without Real-Debrid (see `App::minerva_keep`).
enum MinervaKeep<'a> {
    All,
    None,
    /// Those TorBox has cached (lowercase file names).
    Cached(&'a HashSet<String>),
}

/// What TorBox has of one Minerva torrent. `None` in the files = every file.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TorrentCache {
    pub files: Option<HashSet<String>>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct CacheStatusFile {
    checked: u64,
    torrents: HashMap<String, TorrentCache>,
}

/// Whether TorBox's cached file list covers a whole Minerva folder (`names`: the folder's
/// files, lowercase). A torrent TorBox packed into one zip lists just that zip.
fn fully_cached(names: &HashSet<String>, cached: &HashSet<String>) -> bool {
    if names.is_empty() {
        return false;
    }
    let packed = cached.len() == 1 && cached.iter().all(|c| library::has_ext(c, "zip") && !names.contains(c));
    // TorBox lists every file of a fully cached torrent; allow a little slack.
    packed || cached.iter().filter(|c| names.contains(*c)).count() * 100 >= names.len() * 98
}

/// How long a TorBox copy found broken is avoided before trying it again.
const BROKEN_RETRY: Duration = Duration::from_hours(168);

fn broken_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("minerva-torbox-broken.json")
}

/// Bytes a file (or a folder, recursively) takes.
fn disk_size(p: &Path) -> u64 {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).map(|d| d.flatten().map(|e| disk_size(&e.path())).sum()).unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

fn cache_status_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("minerva-torbox-cache.json")
}

/// Lower is better: prefer clean USA/World dumps over betas, hacks, other regions.
fn rank(name: &str) -> (u8, u8, usize) {
    let n = name.to_lowercase();
    let junk = u8::from(
        ["(beta", "(proto", "(demo", "(sample", "(pirate", "(hack", "[b]", "[h", "(unl)", "(aftermarket", "(kiosk", "(debug"]
            .iter()
            .any(|j| n.contains(j)),
    );
    // region tags: "(USA)", "(Japan, USA)", "(USA, Europe)", "(World)"
    let tags: String = n.split('(').skip(1).map(|t| t.split(')').next().unwrap_or("")).collect::<Vec<_>>().join(",");
    // MAME titles put a date after the region: "(USA 940323)", "(World 910522)"
    let first = |t: &str| t.split_whitespace().next().unwrap_or("").to_string();
    let region = if tags.split(',').any(|t| matches!(first(t).as_str(), "usa" | "world" | "us")) {
        0
    } else if tags.split(',').any(|t| matches!(first(t).as_str(), "europe" | "en")) {
        1
    } else {
        2
    };
    (junk, region, n.len())
}

/// Disc-era and modern systems: a game is never a few MB.
const BIG_GAME_SYSTEMS: &[&str] = &[
    "switch",
    "wiiu",
    "wii",
    "gc",
    "ps3",
    "ps2",
    "psx",
    "psp",
    "psvita",
    "n3ds",
    "xbox",
    "xbox360",
    "dreamcast",
    "saturn",
    "segacd",
    "pcenginecd",
    "neogeocd",
    "3do",
    "cdimono1",
    "atarijaguarcd",
    "pcfx",
];

/// Live uploads are noisy (save files, event data, mods): for big-game systems a file
/// whose extension doesn't identify it as a ROM must be at least 20 MB.
fn plausible_live_rom(name: &str, size: u64, system: &str) -> bool {
    systems::guess_from_filename(name).is_some() || !BIG_GAME_SYSTEMS.contains(&system) || size >= 20 * 1024 * 1024
}

fn matches_all(hay: &str, terms: &[String]) -> bool {
    let h = hay.to_lowercase();
    terms.iter().all(|t| h.contains(t))
}

/// Search text for one set's files.
fn build_hay(s: &SetRef, idx: &ItemIndex, mame_titles: &HashMap<String, String>) -> SetHay {
    let set_system = if s.system.is_empty() { idx.system.clone().unwrap_or_default() } else { s.system.clone() };
    let set_system = systems::folder_static(&set_system);
    let files = idx
        .files
        .iter()
        .map(|f| {
            // File's own extension / folders first, then the set's system.
            let system = systems::folder_static(file_system(f, set_system));
            let title = if crate::mame::is_mame_system(system) {
                crate::mame::short_name(&f.name).and_then(|n| mame_titles.get(&n).cloned())
            } else {
                f.title.clone()
            };
            let mut text = f.name.to_lowercase();
            if let Some(t) = &title {
                text.push(' ');
                text.push_str(&t.to_lowercase());
            }
            (text, system, systems::label_lower(system), title)
        })
        .collect();
    SetHay { set_text: format!("{} {}", idx.title, s.item).to_lowercase(), files }
}

pub struct App {
    pub tab: Tab,
    pub editing: bool,
    pub cfg: Config,
    pub cache_dir: PathBuf,

    // sets for search
    pub sets: Vec<SetRef>,
    pub index: HashMap<String, Arc<ItemIndex>>,
    pub indexing: HashSet<String>,
    index_permits: Arc<Semaphore>,
    minerva_pending: usize,
    /// Zipped ROM-set torrents in the TorBox account, by info-hash.
    tb_zips: HashMap<String, tbzips::Candidate>,
    tb_zips_pending: bool,
    /// Torrents in the TorBox account and what of each is there (for ⚡ in search).
    tb_held: TorboxHeld,
    /// btih → what TorBox has cached of that Minerva torrent (absent = not checked yet).
    pub minerva_cache: HashMap<String, TorrentCache>,
    cache_checked: u64,
    cache_check_running: bool,
    rd_last_poll: Option<Instant>,
    rd_polls_in_flight: usize,
    /// Games waiting to download (bulk download with D), oldest first.
    pub download_queue: std::collections::VecDeque<String>,

    // search
    pub rom_query: String,
    local_hits: Vec<RomHit>,
    live_hits: Vec<RomHit>,
    vimm_hits: Vec<RomHit>,
    /// Vimm games opened in the browser this session (game id → when).
    pub vimm_waiting: HashMap<String, std::time::SystemTime>,
    /// Where the browser saves files; watched for Vimm downloads.
    pub downloads_dir: PathBuf,
    /// Download sizes seen on the last scan (a file counts as finished once its size holds).
    dl_sizes: HashMap<PathBuf, u64>,
    /// Downloads already being (or done being) imported.
    dl_claimed: HashSet<PathBuf>,
    last_dl_scan: Option<Instant>,
    /// Every hit (one per URL); `search_rows` groups them.
    pub rom_hits: Vec<RomHit>,
    pub search_rows: Vec<SearchRow>,
    /// Each search row's key (system header or game), for keeping the selection.
    search_keys: Vec<String>,
    /// Systems folded in the search results.
    search_collapsed: HashSet<String>,
    /// Selected row of `search_rows`.
    pub rom_state: ListState,
    live_gen: u64,
    live_due: Option<Instant>,
    pub live_loading: bool,

    // library
    pub lib: Library,
    pub lib_filter: String,
    pub lib_paste: String,
    pub lib_input: LibInput,
    pub lib_cursor: Option<RowKey>,
    pub lib_sort: LibSort,
    /// systems whose games are hidden
    pub collapsed: HashSet<String>,
    auto_collapsed: bool,
    pub lib_state: ListState,
    pub remote: HashMap<String, Remote>,
    pub local: HashMap<String, LocalState>,
    pub present: HashSet<String>,
    /// PS3 games on disk that RPCS3 can't run yet (zip to unpack, or no `.dkey`).
    pub needs_key: HashSet<String>,
    /// MAME short name → full title ("sf2" → "Street Fighter II …").
    pub mame_titles: Arc<HashMap<String, String>>,
    /// Minerva torrents whose TorBox copy turned out unreadable (btih → when; unix secs).
    broken_torbox: HashMap<String, u64>,
    /// `X` pressed once on this game: a second `X` (within 5 s) deletes its files.
    confirm_delete: Option<(String, Instant)>,
    /// Search text per indexed set; dropped when the set's index or the MAME titles change.
    hay: HashMap<String, SetHay>,
    /// A background worker is building search text (see `start_hays`).
    hay_worker: bool,
    /// Parsed file names by "system\0name" (see `merge_hits`).
    names: HashMap<String, Arc<NameInfo>>,
    /// Search text arrived since the worker started: refresh the results when it's done.
    hay_fresh: bool,
    /// Bumped when the MAME titles change: search text built before is stale for MAME sets.
    mame_gen: u64,
    mame_pending: bool,
    /// "system/file name" (lowercase) of everything on disk, for marking search results.
    pub on_disk: HashSet<String>,
    /// Bytes each downloaded game takes in the roms folder (what's really there, which can
    /// differ from the source's size: unzipped discs, PS3 folders). Games not on disk are absent.
    pub disk_size: HashMap<String, u64>,
    pub scanning: bool,
    retry_at: HashMap<String, Instant>,
    last_poll: Option<Instant>,
    polls_in_flight: usize,

    pub status: String,
    pub quit: bool,
    /// The controller was used last (and is still connected): the help line shows buttons
    /// instead of keys. A keypress flips it back, so SSHing in while a pad is plugged in works.
    pub pad: bool,
    /// The text box was opened from the controller, so its typing comes from Steam's on-screen
    /// keyboard as key events and shouldn't switch the help line back to keys.
    pad_typing: bool,
    pub menu: Option<Menu>,
    pub tex: TexTab,
    /// Sources tab: cursor (a group name or set item) and the groups opened.
    pub sets_cursor: Option<String>,
    pub sets_state: ListState,
    sets_open: HashSet<String>,
    /// Why a set's last indexing failed.
    index_errors: HashMap<String, String>,
    /// Set torrents sent to a debrid service to cache whole (service, info-hash).
    pub cache_jobs: HashMap<(Debrid, String), CacheJob>,
    /// Where `cfg` came from, for saving settings changed in the app (None in tests).
    pub config_file: Option<PathBuf>,
    pub cfg_tab: CfgTab,
    /// Quit and start again (settings that only apply at startup changed).
    pub restart: bool,
    tx: UnboundedSender<Msg>,
    local_tx: LocalTx,
    ctx: jobs::Ctx,
}

impl App {
    pub fn new(cfg: Config, cache_dir: PathBuf, lib: Library, tx: UnboundedSender<Msg>, local_tx: LocalTx, ctx: jobs::Ctx) -> Self {
        let downloads_dir = cfg.downloads_dir.clone().unwrap_or_else(vimm::default_downloads_dir);
        let tex = TexTab::new(&ctx.roms_dir);
        let mut sets: Vec<SetRef> = Vec::new();
        // Minerva first: it wins dedupe for files TorBox has cached (others are hidden).
        let mut minerva_stale = Vec::new();
        if cfg.use_minerva {
            for group in minerva::GROUPS {
                match load_folders(&cache_dir, group) {
                    Some((folders, fresh)) => {
                        sets.extend(minerva_sets(group, &folders));
                        if !fresh {
                            minerva_stale.push(group.to_string());
                        }
                    }
                    None => minerva_stale.push(group.to_string()),
                }
            }
        }
        // TorBox zips next: instant, since TorBox already has them.
        let tb_zips: Vec<tbzips::Candidate> = if cfg.use_torbox_zips && ctx.torbox.is_some() {
            std::fs::read(tb_zips_path(&cache_dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
        } else {
            Vec::new()
        };
        sets.extend(tb_zip_sets(&tb_zips));
        if cfg.use_default_sets {
            sets.extend(sources::default_sets().into_iter().map(|(item, s)| SetRef { item, system: s.to_string() }));
        }
        if cfg.use_ifarchive {
            // per-file extensions pick zmachine; everything else is the "if" system
            sets.push(SetRef { item: ifarchive::KEY.into(), system: "if".into() });
        }
        for c in &cfg.extra_sets {
            if !sets.iter().any(|s| s.item == c.item) {
                sets.push(SetRef { item: c.item.clone(), system: c.system.clone() });
            }
        }
        let mut app = Self {
            tab: Tab::Search,
            editing: lib.games.is_empty(),
            cfg,
            cache_dir,
            sets,
            index: HashMap::new(),
            indexing: HashSet::new(),
            index_permits: Arc::new(Semaphore::new(4)),
            minerva_pending: 0,
            tb_zips_pending: false,
            tb_held: HashMap::new(),
            tb_zips: tb_zips.into_iter().map(|c| (c.hash.clone(), c)).collect(),
            minerva_cache: HashMap::new(),
            cache_checked: 0,
            cache_check_running: false,
            rd_last_poll: None,
            rd_polls_in_flight: 0,
            download_queue: std::collections::VecDeque::new(),
            rom_query: String::new(),
            local_hits: Vec::new(),
            live_hits: Vec::new(),
            vimm_hits: Vec::new(),
            vimm_waiting: HashMap::new(),
            downloads_dir,
            dl_sizes: HashMap::new(),
            dl_claimed: HashSet::new(),
            last_dl_scan: None,
            rom_hits: Vec::new(),
            search_rows: Vec::new(),
            search_keys: Vec::new(),
            search_collapsed: HashSet::new(),
            rom_state: ListState::default(),
            live_gen: 0,
            live_due: None,
            live_loading: false,
            lib,
            lib_filter: String::new(),
            lib_paste: String::new(),
            lib_input: LibInput::Filter,
            lib_cursor: None,
            lib_sort: LibSort::Name,
            collapsed: HashSet::new(),
            auto_collapsed: false,
            lib_state: ListState::default(),
            remote: HashMap::new(),
            local: HashMap::new(),
            present: HashSet::new(),
            needs_key: HashSet::new(),
            mame_titles: Arc::default(),
            broken_torbox: HashMap::new(),
            confirm_delete: None,
            hay: HashMap::new(),
            hay_worker: false,
            names: HashMap::new(),
            hay_fresh: false,
            mame_gen: 0,
            mame_pending: false,
            on_disk: HashSet::new(),
            disk_size: HashMap::new(),
            scanning: false,
            retry_at: HashMap::new(),
            last_poll: None,
            polls_in_flight: 0,
            status: String::new(),
            quit: false,
            pad: false,
            pad_typing: false,
            menu: None,
            tex,
            sets_cursor: None,
            sets_state: ListState::default(),
            sets_open: HashSet::new(),
            index_errors: HashMap::new(),
            cache_jobs: HashMap::new(),
            config_file: None,
            cfg_tab: CfgTab::default(),
            restart: false,
            tx,
            local_tx,
            ctx,
        };

        app.apply_groups_off();
        let items: Vec<String> = app.sets.iter().map(|s| s.item.clone()).collect();
        for item in items {
            match archive::load_cached(&app.cache_dir, &item) {
                Some(idx) => {
                    let stale = idx.age() > REINDEX_AFTER;
                    app.index.insert(item.clone(), Arc::new(idx));
                    if stale {
                        app.start_index(item);
                    }
                }
                None => app.start_index(item),
            }
        }
        // Games added from live search: their set's cached index says whether it's
        // restricted (and so must be downloaded directly).
        let extra: HashSet<String> = app
            .lib
            .games
            .iter()
            .filter_map(|g| archive::item_of_url(&g.source))
            .filter(|i| !app.index.contains_key(*i))
            .map(str::to_string)
            .collect();
        for item in extra {
            if let Some(idx) = archive::load_cached(&app.cache_dir, &item) {
                app.index.insert(item, Arc::new(idx));
            }
        }

        for group in minerva_stale {
            app.refresh_minerva(group);
        }
        app.refresh_tb_zips();
        if let Some(c) = std::fs::read_to_string(cache_status_path(&app.cache_dir))
            .ok()
            .and_then(|t| serde_json::from_str::<CacheStatusFile>(&t).ok())
            .filter(|c| now_secs().saturating_sub(c.checked) < CACHE_STATUS_REFRESH.as_secs())
        {
            app.minerva_cache = c.torrents;
            app.cache_checked = c.checked;
        }
        app.broken_torbox = std::fs::read(broken_path(&app.cache_dir))
            .ok()
            .and_then(|b| serde_json::from_slice::<HashMap<String, u64>>(&b).ok())
            .unwrap_or_default();
        app.broken_torbox.retain(|_, t| now_secs().saturating_sub(*t) < BROKEN_RETRY.as_secs());
        app.check_minerva_cache();
        if app.sets.iter().any(|s| crate::mame::is_mame_system(&s.system)) {
            app.mame_pending = true;
            let (http, dir, tx) = (app.ctx.http.clone(), app.cache_dir.clone(), app.tx.clone());
            tokio::spawn(async move {
                let _ = tx.send(Msg::MameTitles(crate::mame::load(&http, &dir).await));
            });
        }

        for g in app.lib.games.iter().filter(|g| g.kind != SourceKind::Local) {
            let r = match (g.torbox_id, app.ctx.torbox.is_some()) {
                _ if app.direct_only(&g.source) => Remote::Direct,
                _ if g.rd_id.is_some() && app.ctx.rd.is_some() => Remote::Pending { state: "checking".into(), progress: 0.0 },
                (_, false) => Remote::NoKey,
                (Some(_), true) => Remote::Pending { state: "checking".into(), progress: 0.0 },
                (None, true) => Remote::NotSent,
            };
            app.remote.insert(g.id.clone(), r);
        }
        app.refresh_presence();
        app.maybe_auto_collapse();
        app.lib_cursor = app.first_row();
        app.start_scan();

        app.status = if app.ctx.torbox.is_none() {
            "No TorBox API key: add one on the Config tab (5). Direct downloads (d) still work.".into()
        } else if !app.indexing.is_empty() {
            format!("indexing {} ROM sets in the background (first run only)…", app.indexing.len())
        } else {
            format!("{} games in library · {} files searchable", app.lib.games.len(), app.total_files())
        };
        app
    }

    /// How a game file shows in lists: MAME short-name zips get their full title.
    pub fn display_name(&self, system: &str, file: &str) -> String {
        let base = file.rsplit('/').next().unwrap_or(file);
        match crate::mame::is_mame_system(system).then(|| crate::mame::short_name(base)).flatten().and_then(|n| self.mame_titles.get(&n)) {
            Some(t) => format!("{t}  [{base}]"),
            None => base.to_string(),
        }
    }

    /// How a library game shows: its own title (IF Archive) or `display_name`.
    pub fn game_name(&self, g: &Game) -> String {
        match &g.title {
            Some(t) => format!("{t}  [{}]", g.name.rsplit('/').next().unwrap_or(&g.name)),
            None => self.display_name(&g.system, &g.name),
        }
    }

    pub fn total_files(&self) -> usize {
        self.sets.iter().filter_map(|s| self.index.get(&s.item)).filter(|i| !self.hidden(i)).map(|i| i.files.len()).sum()
    }

    /// Still discovering or indexing sets.
    pub fn busy(&self) -> bool {
        !self.indexing.is_empty() || self.minerva_pending > 0 || self.mame_pending || self.tb_zips_pending
    }

    pub fn indexed_sets(&self) -> usize {
        self.sets.iter().filter(|s| self.index.contains_key(&s.item)).count()
    }

    fn save_lib(&mut self) {
        if let Err(e) = self.lib.save() {
            self.status = format!("failed to save library: {e:#}");
        }
    }

    fn refresh_presence(&mut self) {
        let roms = &self.ctx.roms_dir;
        self.needs_key = self.lib.games.iter().filter(|g| crate::ps3::needs_setup(g, roms).is_some()).map(|g| g.id.clone()).collect();
        self.present =
            self.lib.games.iter().filter(|g| g.is_present(roms) || self.needs_key.contains(&g.id)).map(|g| g.id.clone()).collect();
        self.on_disk = self.lib.games.iter().filter(|g| self.present.contains(&g.id)).flat_map(|g| g.local_keys(roms)).collect();
        self.disk_size = self
            .lib
            .games
            .iter()
            .filter(|g| self.present.contains(&g.id))
            .map(|g| (g.id.clone(), self.files_on_disk(g).iter().map(|p| disk_size(p)).sum()))
            .collect();
    }

    /// Bytes the game takes on disk; 0 when it isn't downloaded.
    pub fn size_on_disk(&self, g: &Game) -> u64 {
        self.disk_size.get(&g.id).copied().unwrap_or(0)
    }

    /// "system/file name" key used to match a search hit against what's on disk.
    pub fn disk_key(system: &str, name: &str) -> String {
        format!("{system}/{}", jobs::sanitize(name).to_lowercase())
    }

    /// Look through the roms folder (in the background) and sync the library with it.
    pub fn start_scan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let roms = self.ctx.roms_dir.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::Scanned(library::scan(&roms)));
        });
    }

    /// Add games found on disk that the library doesn't know; drop local-only entries
    /// whose files are gone. Games from a source keep their entry either way.
    fn sync_with_disk(&mut self, found: Vec<library::Found>) {
        let roms = self.ctx.roms_dir.clone();
        let found_keys: HashSet<String> = found.iter().map(|f| f.rel.to_lowercase()).collect();
        // Track files (a .cue's .bins) belong to their sheet's game, not to entries of their own.
        let track_keys: HashSet<String> = found.iter().flat_map(|f| f.tracks.iter().map(|t| t.to_lowercase())).collect();
        let before = self.lib.games.len();
        self.lib.games.retain(|g| {
            let keys = g.local_keys(&roms);
            g.kind != SourceKind::Local
                || (keys.iter().all(|k| found_keys.contains(k) || track_keys.contains(k)) && !keys.iter().all(|k| track_keys.contains(k)))
        });
        let gone = before - self.lib.games.len();
        let known: HashSet<String> = self.lib.games.iter().flat_map(|g| g.local_keys(&roms)).collect();
        let mut added = 0;
        for f in found.into_iter().filter(|f| !known.contains(&f.rel.to_lowercase())) {
            let mut g = Game::local(f.system, f.rel, f.size);
            g.local_files.extend(f.tracks);
            self.lib.games.push(g);
            added += 1;
        }
        if added > 0 || gone > 0 {
            self.save_lib();
        }
        self.refresh_presence();
        self.maybe_auto_collapse();
        self.fix_cursor();
        let total = self.present.len();
        self.status = format!("roms folder: {total} games on disk ({added} new, {gone} gone) · {}", roms.display());
    }

    // ---------- background work ----------

    /// Called every frame.
    pub fn tick(&mut self) {
        if self.live_due.is_some_and(|t| Instant::now() >= t) {
            self.live_due = None;
            self.start_live_search();
        }
        self.start_hays();
        self.auto_send();
        self.poll_torbox();
        self.poll_rd();
        self.pump_downloads();
        self.watch_downloads();
    }

    fn save_config(&mut self) {
        if let Some(path) = &self.config_file
            && let Err(e) = self.cfg.save(path)
        {
            self.status = format!("{} · couldn't save the config: {e:#}", self.status);
        }
    }

    /// Vimm can't be downloaded from directly: open the game's page in a browser.
    fn open_vimm(&mut self, g: &Game) {
        if g.system.is_empty() {
            self.status = "pick a system first".into();
            return;
        }
        match vimm::open_in_browser(&g.source, self.cfg.browser.as_deref()) {
            Ok(browser) => {
                self.vimm_waiting.insert(g.id.clone(), std::time::SystemTime::now());
                self.status = format!(
                    "opened in {browser}: press Download there · it goes to {}/ when it lands in {}",
                    g.system,
                    self.downloads_dir.display()
                );
            }
            Err(e) => self.status = format!("{e:#} · set `browser` in the config, or open {} yourself", g.source),
        }
    }

    /// Every 2 s while Vimm games are waiting: look for finished downloads in the
    /// downloads folder and import the ones that match a game.
    fn watch_downloads(&mut self) {
        if self.last_dl_scan.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        self.last_dl_scan = Some(Instant::now());
        let busy = |id: &str| matches!(self.local.get(id), Some(LocalState::Downloading { .. }));
        let pending: Vec<Game> = self
            .lib
            .games
            .iter()
            .filter(|g| g.kind == SourceKind::Vimm && !g.system.is_empty() && !self.present.contains(&g.id) && !busy(&g.id))
            .cloned()
            .collect();
        if pending.is_empty() {
            self.dl_sizes.clear();
            return;
        }
        let files = vimm::finished_downloads(&self.downloads_dir);
        let prev = std::mem::take(&mut self.dl_sizes);
        self.dl_sizes = files.iter().map(|f| (f.path.clone(), f.size)).collect();
        let mut taken: HashSet<String> = HashSet::new();
        for f in files {
            if prev.get(&f.path) != Some(&f.size) || self.dl_claimed.contains(&f.path) {
                continue; // still growing (or new this scan), or already handled
            }
            let file_name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let by_title = pending
                .iter()
                .filter(|g| !taken.contains(&g.id))
                .filter_map(|g| vimm::match_score(&g.name, &file_name).map(|s| (s, g)))
                .max_by_key(|(s, _)| *s)
                .map(|(_, g)| g);
            // A name that doesn't match: take it only for the one game opened just before it.
            let by_time = || {
                let opened: Vec<&Game> = pending
                    .iter()
                    .filter(|g| !taken.contains(&g.id))
                    .filter(|g| self.vimm_waiting.get(&g.id).is_some_and(|t| f.modified >= *t))
                    .collect();
                (opened.len() == 1 && vimm::looks_like_game(&file_name)).then(|| opened[0])
            };
            let Some(g) = by_title.or_else(by_time).cloned() else { continue };
            taken.insert(g.id.clone());
            self.dl_claimed.insert(f.path.clone());
            self.vimm_waiting.remove(&g.id);
            self.status = format!("found {file_name} in downloads · moving it to {}/", g.system);
            self.local.insert(g.id.clone(), LocalState::Downloading { file: file_name, done: 0, total: f.size });
            tokio::spawn(jobs::import_download(g, f.path, self.ctx.clone(), self.local_tx.clone()));
        }
    }

    /// Queue every library game that's ready on TorBox / Real-Debrid and not on disk.
    fn download_all_ready(&mut self) {
        let ready: Vec<String> = self
            .lib
            .games
            .iter()
            .filter(|g| matches!(self.remote.get(&g.id), Some(Remote::Ready | Remote::Direct)))
            .filter(|g| !self.present.contains(&g.id) && !matches!(self.local.get(&g.id), Some(LocalState::Downloading { .. })))
            .filter(|g| !g.system.is_empty() && !self.download_queue.contains(&g.id))
            .map(|g| g.id.clone())
            .collect();
        self.status = match ready.len() {
            0 if self.download_queue.is_empty() => "nothing is ready to download".into(),
            0 => format!("{} already queued", self.download_queue.len()),
            n => format!("queued {n} ready game(s); {MAX_DOWNLOADS} download at a time"),
        };
        self.download_queue.extend(ready);
        self.pump_downloads();
    }

    fn pump_downloads(&mut self) {
        while self.local.values().filter(|s| matches!(s, LocalState::Downloading { .. })).count() < MAX_DOWNLOADS {
            let Some(id) = self.download_queue.pop_front() else { break };
            let Some(g) = self.lib.get(&id).cloned() else { continue };
            let direct = match self.remote.get(&id) {
                _ if self.present.contains(&id) => continue, // already there
                Some(Remote::Ready) => false,
                Some(Remote::Direct) => true,
                _ => continue, // removed, or no longer ready
            };
            self.start_local(g, !direct);
        }
    }

    /// Add Metacritic's best-reviewed games for the cursor's system.
    fn add_top_games(&mut self) {
        let Some(system) = self.cursor_system().filter(|s| !s.is_empty()) else { return };
        match crate::metacritic::top_list(&system) {
            None => self.status = format!("no top-games list for {}", systems::label(&system)),
            Some(crate::metacritic::TopList::Classics(titles)) => self.on_msg(Msg::TopGames(system, Ok(titles))),
            Some(crate::metacritic::TopList::Metacritic(platform)) => {
                self.status = format!("asking Metacritic for the top {TOP_N} {} games…", systems::label(&system));
                let (http, tx) = (self.ctx.http.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let _ = tx.send(Msg::TopGames(system, crate::metacritic::top(&http, platform, TOP_N).await));
                });
            }
        }
    }

    /// The best indexed file for each title (same system, same normalized name). Titles
    /// with no usable copy map to None.
    /// The best indexed file for each title (same system, same normalized name). A title
    /// with no exact match may match "<title> - <subtitle>" (No-Intro's "Wave Race 64 -
    /// Kawasaki Jet Ski"), but only when a single game fits. No usable copy = None.
    pub fn best_hits_for(&self, system: &str, titles: &[String]) -> Vec<(String, Option<RomHit>)> {
        let norms: Vec<String> = titles.iter().map(|t| crate::metacritic::norm(t)).collect();
        let wanted: HashMap<&str, usize> = norms.iter().enumerate().map(|(i, t)| (t.as_str(), i)).collect();
        let base = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
        let keep = |slot: &mut Option<RomHit>, hit: RomHit| {
            if slot.as_ref().is_none_or(|b| rank(&base(&hit.name)) < rank(&base(&b.name))) {
                *slot = Some(hit);
            }
        };
        let mut exact: Vec<Option<RomHit>> = vec![None; titles.len()];
        // per title: the distinct longer names seen, and the best file among them
        let mut longer: Vec<(HashSet<String>, Option<RomHit>)> = vec![(HashSet::new(), None); titles.len()];
        for s in &self.sets {
            let Some(idx) = self.index.get(&s.item) else { continue };
            if self.hidden(idx) || !self.cfg.source_on(&s.item) {
                continue;
            }
            let set_system = if s.system.is_empty() { idx.system.clone().unwrap_or_default() } else { s.system.clone() };
            for f in &idx.files {
                if file_system(f, &set_system) != system {
                    continue;
                }
                let n = crate::metacritic::norm(&f.name);
                let exact_i = wanted.get(n.as_str()).copied();
                let longer_i: Vec<usize> = if exact_i.is_some() {
                    Vec::new()
                } else {
                    norms.iter().enumerate().filter(|(_, t)| n.starts_with(&format!("{t} "))).map(|(i, _)| i).collect()
                };
                if exact_i.is_none() && longer_i.is_empty() {
                    continue;
                }
                if rank(&base(&f.name)).0 == 1 {
                    continue; // betas, demos, hacks
                }
                if self.ctx.rd.is_none() && idx.btih.as_deref().is_some_and(|h| !self.minerva_available(h, &f.name)) {
                    continue;
                }
                let (url, torrent) = match &idx.btih {
                    Some(h) => (format!("minerva:{h}/{}", f.name), Some((h.clone(), f.torrent_index))),
                    None => (f.url(&s.item), None),
                };
                let source = if idx.title.is_empty() { s.item.clone() } else { idx.title.clone() };
                let hit = RomHit {
                    url,
                    name: f.name.clone(),
                    size: f.size,
                    system: system.to_string(),
                    source,
                    live: false,
                    torrent,
                    title: None,
                };
                if let Some(i) = exact_i {
                    keep(&mut exact[i], hit);
                } else {
                    for i in longer_i {
                        longer[i].0.insert(n.clone());
                        keep(&mut longer[i].1, hit.clone());
                    }
                }
            }
        }
        let picked = exact.into_iter().zip(longer).map(|(e, (names, l))| e.or(if names.len() == 1 { l } else { None }));
        titles.iter().cloned().zip(picked).collect()
    }

    fn game_from_hit(hit: &RomHit) -> Game {
        let name = jobs::sanitize(&hit.name);
        match &hit.torrent {
            Some((h, index)) => Game::torrent_file(hit.url.clone(), name, hit.system.clone(), minerva::magnet(h), *index, hit.size),
            None if vimm::is_vimm_url(&hit.url) => Game::new(hit.name.clone(), hit.system.clone(), SourceKind::Vimm, hit.url.clone(), 0),
            None => {
                let mut g = Game::new(name, hit.system.clone(), SourceKind::Link, hit.url.clone(), hit.size);
                if ifarchive::is_url(&hit.url) {
                    g.title.clone_from(&hit.title);
                }
                g
            }
        }
    }

    /// Every game in the library should be on TorBox (unless it's already on disk).
    fn auto_send(&mut self) {
        if self.ctx.torbox.is_none() && self.ctx.rd.is_none() {
            return;
        }
        let sending = self.remote.values().filter(|r| matches!(r, Remote::Sending)).count();
        if sending >= MAX_SENDING {
            return;
        }
        let now = Instant::now();
        let due: Vec<String> = self
            .lib
            .games
            .iter()
            .filter(|g| !matches!(g.kind, SourceKind::Local | SourceKind::Vimm) && !self.present.contains(&g.id))
            .filter(|g| !ifarchive::is_url(&g.source))
            .filter(|g| match self.remote.get(&g.id) {
                Some(Remote::NotSent | Remote::Missing) | None => true,
                Some(Remote::Error(_)) => self.retry_at.get(&g.id).is_none_or(|t| now >= *t),
                _ => false,
            })
            .take(MAX_SENDING - sending)
            .map(|g| g.id.clone())
            .collect();
        for id in due {
            self.send_to_torbox(&id);
        }
    }

    /// Poll TorBox for games that are on TorBox but not yet on disk.
    fn poll_torbox(&mut self) {
        let Some(tb) = self.ctx.torbox.clone() else { return };
        let waiting: Vec<&Game> = self.lib.games.iter().filter(|g| g.torbox_id.is_some() && !self.present.contains(&g.id)).collect();
        let any_pending = waiting.iter().any(|g| !matches!(self.remote.get(&g.id), Some(Remote::Ready)));
        let interval = if any_pending { self.ctx.poll } else { Duration::from_secs(60) };
        if self.polls_in_flight > 0 || self.last_poll.is_some_and(|t| t.elapsed() < interval) {
            return;
        }
        let mut kinds: HashSet<Kind> = waiting.iter().map(|g| g.torbox_kind()).collect();
        if self.cache_jobs.iter().any(|((d, _), j)| *d == Debrid::TorBox && !j.done) {
            kinds.insert(Kind::Torrent);
        }
        self.last_poll = Some(Instant::now());
        self.refresh_presence();
        for kind in kinds {
            self.polls_in_flight += 1;
            let tb = tb.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(Msg::Remote(kind, tb.list(kind).await));
            });
        }
    }

    /// Minerva games TorBox can't deliver go to Real-Debrid, which can download one file
    /// out of a folder torrent. Everything else stays on TorBox.
    fn uses_rd(&self, g: &Game) -> bool {
        if self.ctx.rd.is_none() {
            return false;
        }
        if g.rd_id.is_some() {
            return true;
        }
        match (&g.pick, g.btih()) {
            (Some(pick), Some(h)) => self.ctx.torbox.is_none() || !self.minerva_available(&h, pick),
            _ => false,
        }
    }

    fn send_to_rd(&mut self, id: &str) {
        let Some(rd) = self.ctx.rd.clone() else { return };
        let Some(g) = self.lib.get(id).cloned() else { return };
        if g.rd_id.is_some() {
            self.rd_last_poll = None; // already there; just check on it
            return;
        }
        let (Some(pick), Some(h)) = (g.pick.clone(), g.btih()) else { return };
        if matches!(self.remote.get(&g.id), Some(Remote::Sending)) {
            return;
        }
        self.remote.insert(g.id.clone(), Remote::Sending);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let res = rd.add_one_file(&g.source, &h, &pick).await;
            let _ = tx.send(Msg::RdSent(g.id.clone(), res));
        });
    }

    /// One listing request covers every game on Real-Debrid (RD rate-limits requests).
    fn poll_rd(&mut self) {
        let Some(rd) = self.ctx.rd.clone() else { return };
        let waiting: Vec<&Game> = self.lib.games.iter().filter(|g| g.rd_id.is_some() && !self.present.contains(&g.id)).collect();
        let any_pending = waiting.iter().any(|g| !matches!(self.remote.get(&g.id), Some(Remote::Ready)));
        let interval = if any_pending { self.ctx.poll.max(Duration::from_secs(15)) } else { Duration::from_secs(120) };
        let caching = self.cache_jobs.iter().any(|((d, _), j)| *d == Debrid::RealDebrid && !j.done);
        let interval = if caching { interval.min(Duration::from_secs(60)) } else { interval };
        if (waiting.is_empty() && !caching) || self.rd_polls_in_flight > 0 || self.rd_last_poll.is_some_and(|t| t.elapsed() < interval) {
            return;
        }
        self.rd_last_poll = Some(Instant::now());
        self.rd_polls_in_flight = 1;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::RdList(rd.list().await));
        });
    }

    /// Ask TorBox which files of each Minerva torrent it has cached. Many big disc-system
    /// torrents are only partly cached (often a single game), so search hides the rest.
    fn check_minerva_cache(&mut self) {
        let Some(tb) = self.ctx.torbox.clone() else { return };
        if self.cache_check_running {
            return;
        }
        let mut hashes: Vec<String> = self
            .index
            .iter()
            .filter(|(k, _)| k.starts_with("minerva:"))
            .filter_map(|(_, i)| i.btih.clone())
            .filter(|h| !self.minerva_cache.contains_key(h))
            .collect();
        hashes.sort();
        hashes.dedup();
        if hashes.is_empty() {
            return;
        }
        self.cache_check_running = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let res = tb.cached_files(&hashes).await;
            let _ = tx.send(Msg::MinervaCache(hashes, res));
        });
    }

    /// TorBox's copy of a Minerva folder can't be read: stop using it for a week. Its games
    /// move to Real-Debrid when there's a key; search shows other sources' copies instead.
    fn torbox_copy_broken(&mut self, btih: &str) {
        self.broken_torbox.insert(btih.to_lowercase(), now_secs());
        if let Ok(json) = serde_json::to_vec(&self.broken_torbox) {
            let _ = std::fs::write(broken_path(&self.cache_dir), json);
        }
        let affected: Vec<String> = self
            .lib
            .games
            .iter()
            .filter(|g| !self.present.contains(&g.id) && g.btih().is_some_and(|h| h.eq_ignore_ascii_case(btih)))
            .map(|g| g.id.clone())
            .collect();
        if self.ctx.rd.is_some() {
            for id in &affected {
                self.local.remove(id);
                if let Some(g) = self.lib.get_mut(id) {
                    g.torbox_id = None;
                }
                self.send_to_rd(id);
            }
            self.save_lib();
            self.status = format!("TorBox's copy of that folder is broken; moved {} game(s) to Real-Debrid", affected.len());
        } else {
            self.status = "TorBox's copy of that folder is broken; remove the game (x) and add another source's copy from Search".into();
        }
        self.refresh_local();
    }

    /// Whether TorBox can deliver this Minerva file: cached, or small enough a folder to
    /// download whole. True when the cache status isn't known yet.
    pub fn minerva_available(&self, btih: &str, file: &str) -> bool {
        if self.broken_torbox.contains_key(&btih.to_lowercase()) {
            return false;
        }
        match self.minerva_cache.get(btih) {
            Some(TorrentCache { files: Some(files) }) => {
                files.contains(&file.rsplit('/').next().unwrap_or(file).to_lowercase()) || self.minerva_small(btih)
            }
            _ => true,
        }
    }

    /// Sets of groups an old `use_minerva = false` (etc.) left out: switched off in the
    /// Sources tab instead, unless already switched there.
    fn apply_groups_off(&mut self) {
        use crate::config::Group;
        if self.cfg.groups_off.is_empty() {
            return;
        }
        let builtin: HashSet<String> = sources::default_sets().into_iter().map(|(item, _)| item).collect();
        let off: Vec<String> = self
            .sets
            .iter()
            .map(|s| &s.item)
            .filter(|item| !self.cfg.sources.contains_key(*item))
            .filter(|item| {
                let group = if minerva::parse_key(item).is_some() {
                    Group::Minerva
                } else if tbzips::parse_key(item).is_some() {
                    Group::TorboxZips
                } else if *item == ifarchive::KEY {
                    Group::IfArchive
                } else if builtin.contains(*item) {
                    Group::DefaultSets
                } else {
                    return false;
                };
                self.cfg.groups_off.contains(&group)
            })
            .cloned()
            .collect();
        for item in off {
            self.cfg.set_source(&item, false);
        }
    }

    /// `minerva_available` for every file of one set at once.
    fn minerva_keep<'a>(&'a self, idx: &ItemIndex) -> MinervaKeep<'a> {
        let Some(h) = idx.btih.as_deref().filter(|_| self.ctx.rd.is_none()) else { return MinervaKeep::All };
        if self.broken_torbox.contains_key(&h.to_lowercase()) {
            return MinervaKeep::None;
        }
        match self.minerva_cache.get(h) {
            Some(TorrentCache { files: Some(files) }) if idx.files.iter().map(|f| f.size).sum::<u64>() > UNCACHED_MAX => {
                MinervaKeep::Cached(files)
            }
            _ => MinervaKeep::All,
        }
    }

    /// A Minerva folder torrent small enough to download uncached.
    fn minerva_small(&self, btih: &str) -> bool {
        self.index
            .values()
            .find(|i| i.btih.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(btih)))
            .is_some_and(|i| i.files.iter().map(|f| f.size).sum::<u64>() <= UNCACHED_MAX)
    }

    /// A readable TorBox name for a Minerva folder torrent ("Minerva - Redump - Sony -
    /// PlayStation 3"); every one of them is otherwise just "Minerva_Myrient".
    /// A restricted archive.org set we can't download from (not logged in).
    fn hidden(&self, idx: &ItemIndex) -> bool {
        idx.restricted && self.cfg.archive_login.is_none()
    }

    /// Downloaded straight from its source, never through TorBox: IF Archive files, and
    /// restricted archive.org sets (only a logged-in account can fetch them).
    pub fn direct_only(&self, source: &str) -> bool {
        ifarchive::is_url(source)
            || self.cfg.archive_login.is_some()
                && archive::item_of_url(source).and_then(|i| self.index.get(i)).is_some_and(|x| x.restricted)
    }

    fn minerva_name(&self, btih: &str) -> Option<String> {
        self.index
            .values()
            .find(|i| i.btih.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(btih)))
            .filter(|i| !i.title.is_empty())
            .map(|i| i.title.replace(" · ", " - "))
    }

    fn start_index(&mut self, item: String) {
        // switched-off sets aren't indexed until they're switched back on
        if !self.cfg.source_on(&item) || !self.indexing.insert(item.clone()) {
            return;
        }
        let http = self.ctx.http.clone();
        let tx = self.tx.clone();
        let permits = self.index_permits.clone();
        let tb_zip = tbzips::parse_key(&item).map(|h| (self.tb_zips.get(h).cloned(), self.ctx.torbox.clone()));
        tokio::spawn(async move {
            let _permit = permits.acquire_owned().await;
            let res = match minerva::parse_key(&item) {
                _ if tb_zip.is_some() => match tb_zip {
                    Some((Some(c), Some(tb))) => tbzips::fetch_index(&tb, &http, &c).await,
                    _ => Err(anyhow::anyhow!("not a TorBox torrent any more")),
                },
                Some((group, folder)) => minerva::fetch_folder(&http, group, folder).await,
                None if item == ifarchive::KEY => ifarchive::fetch_index(&http).await,
                None => archive::fetch_index(&http, &item).await,
            };
            let _ = tx.send(Msg::Indexed(item, res));
        });
    }

    /// Look for zipped ROM-set torrents in the TorBox account (the user adds them there).
    fn refresh_tb_zips(&mut self) {
        let Some(tb) = self.ctx.torbox.clone().filter(|_| self.cfg.use_torbox_zips) else { return };
        let tx = self.tx.clone();
        self.tb_zips_pending = true;
        tokio::spawn(async move {
            let res = tb.list(Kind::Torrent).await.map(|items| (tbzips::candidates(&items), tbzips::held(&items), tbzips::pending(&items)));
            let _ = tx.send(Msg::TorboxZips(res));
        });
    }

    /// Re-read a Minerva group's folder list (new systems appear there over time).
    fn refresh_minerva(&mut self, group: String) {
        self.minerva_pending += 1;
        let http = self.ctx.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let res = minerva::list_group(&http, &group).await;
            let _ = tx.send(Msg::MinervaFolders(group, res));
        });
    }

    fn start_live_search(&mut self) {
        // System words ("ps3") filter afterwards; archive.org only gets the title words.
        let q = self.query().text_query();
        if q.len() < 3 {
            return;
        }
        self.live_gen += 1;
        self.live_loading = true;
        let generation = self.live_gen;
        let http = self.ctx.http.clone();
        let tx = self.tx.clone();
        let restricted_ok = self.cfg.archive_login.is_some();
        // "vimm" in the query only narrows results to Vimm; the sites don't get it.
        let q: String = q.split_whitespace().filter(|w| !w.eq_ignore_ascii_case("vimm")).collect::<Vec<_>>().join(" ");
        if self.cfg.source_on(LIVE_VIMM) && !q.is_empty() {
            let (http, tx, q) = (http.clone(), tx.clone(), q.clone());
            tokio::spawn(async move {
                let _ = tx.send(Msg::Vimm(generation, vimm::search(&http, &q).await));
            });
        }
        if !self.cfg.source_on(LIVE_ARCHIVE) {
            self.live_loading = false;
            return;
        }
        tokio::spawn(async move {
            let _ = tx.send(Msg::Live(generation, archive::search_games(&http, &q, restricted_ok).await));
        });
    }

    fn send_to_torbox(&mut self, id: &str) {
        if self.lib.get(id).is_some_and(|g| self.direct_only(&g.source)) {
            self.remote.insert(id.to_string(), Remote::Direct);
            return;
        }
        if self.lib.get(id).is_some_and(|g| self.uses_rd(g)) {
            self.send_to_rd(id);
            return;
        }
        let Some(tb) = self.ctx.torbox.clone() else {
            self.remote.insert(id.to_string(), Remote::NoKey);
            return;
        };
        let Some(g) = self.lib.get(id).cloned() else { return };
        if matches!(g.kind, SourceKind::Local | SourceKind::Vimm) {
            return;
        }
        let hash = g.btih();
        if let (Some(pick), Some(h)) = (&g.pick, &hash)
            && !self.minerva_available(h, pick)
        {
            self.retry_at.insert(g.id.clone(), Instant::now() + PARTIAL_RETRY);
            self.remote.insert(
                g.id.clone(),
                Remote::Error("not cached on TorBox (it has only part of this Minerva torrent); search for another copy".into()),
            );
            return;
        }
        if g.pick.is_none()
            && let Some(why) = hash.as_deref().and_then(|h| self.whole_minerva_folder(h))
        {
            self.retry_at.insert(g.id.clone(), Instant::now() + PARTIAL_RETRY);
            self.remote.insert(g.id, Remote::Error(format!("{why} (x removes it)")));
            return;
        }
        // Games from the same torrent (e.g. a Minerva system folder) share one TorBox item.
        if let Some(h) = &hash {
            let siblings: Vec<&Game> = self.lib.games.iter().filter(|o| o.id != g.id && o.btih().as_ref() == Some(h)).collect();
            if let Some(tid) = siblings.iter().find_map(|o| o.torbox_id) {
                self.remote.insert(g.id.clone(), Remote::Sending);
                let _ = self.tx.send(Msg::Sent(g.id.clone(), Ok(tid)));
                return;
            }
            if siblings.iter().any(|o| matches!(self.remote.get(&o.id), Some(Remote::Sending))) {
                return; // that request's result is shared with this game when it lands
            }
        }
        self.remote.insert(g.id.clone(), Remote::Sending);
        let tx = self.tx.clone();
        // Never start a multi-TB download for one game: big Minerva torrents must be cached.
        let only_cached = g.pick.is_some() && !hash.as_deref().is_some_and(|h| self.minerva_small(h));
        let torrent_name = hash.as_deref().and_then(|h| self.minerva_name(h));
        tokio::spawn(async move {
            let name = if g.kind == SourceKind::Link { Some(g.name.as_str()) } else { torrent_name.as_deref() };
            let res = match tb.create(g.torbox_kind(), &g.source, name, only_cached).await {
                Ok(id) => Ok(id),
                Err(e) => match &hash {
                    // Maybe it's already in the account (duplicate): find it by hash.
                    Some(h) => match tb.list(Kind::Torrent).await {
                        Ok(items) => items.iter().find(|i| i.hash.eq_ignore_ascii_case(h)).map(|i| i.id).ok_or(e),
                        Err(_) => Err(e),
                    },
                    None => Err(e),
                },
            };
            let res = res.map_err(|e| if only_cached { e.context("not cached on TorBox") } else { e });
            let _ = tx.send(Msg::Sent(g.id.clone(), res));
        });
    }

    fn start_local(&mut self, game: Game, via_torbox: bool) {
        self.local.insert(game.id.clone(), LocalState::Downloading { file: game.name.clone(), done: 0, total: game.size });
        tokio::spawn(jobs::fetch(game, via_torbox, self.ctx.clone(), self.local_tx.clone()));
    }

    pub fn on_msg(&mut self, msg: Msg) {
        if matches!(msg, Msg::TorboxZips(_)) {
            self.tb_zips_pending = false;
        }
        match msg {
            Msg::Indexed(item, res) => {
                self.indexing.remove(&item);
                match res {
                    Ok(idx) => {
                        self.index_errors.remove(&item);
                        let _ = archive::save_cached(&self.cache_dir, &item, &idx);
                        self.hay.remove(&item);
                        self.index.insert(item, Arc::new(idx));
                        if self.indexing.is_empty() {
                            self.status = format!("search ready: {} files across {} sets", self.total_files(), self.indexed_sets());
                            self.check_minerva_cache();
                        }
                        self.refresh_local();
                    }
                    // Keep any cached copy; just report.
                    Err(e) => {
                        self.status = format!("indexing {item} failed: {e:#}");
                        self.index_errors.insert(item, format!("{e:#}"));
                    }
                }
            }
            Msg::TopGames(system, res) => match res {
                Err(e) => self.status = format!("Metacritic: {e:#}"),
                Ok(titles) => {
                    let (mut added, mut had, mut missing) = (0, 0, Vec::new());
                    for (title, hit) in self.best_hits_for(&system, &titles) {
                        let Some(hit) = hit else {
                            missing.push(title);
                            continue;
                        };
                        let base = hit.name.rsplit('/').next().unwrap_or(&hit.name);
                        if self.lib.contains(&hit.url) || self.on_disk.contains(&Self::disk_key(&hit.system, base)) {
                            had += 1;
                            continue;
                        }
                        self.add_game(Self::game_from_hit(&hit));
                        added += 1;
                    }
                    let label = systems::label(&system);
                    let missing = if missing.is_empty() { String::new() } else { format!("; not found: {}", missing.join(", ")) };
                    let from = if crate::metacritic::platform(&system).is_some() { "Metacritic top" } else { "top" };
                    self.status = format!("{from} {}: added {added} {label} games, {had} already in library{missing}", titles.len());
                }
            },
            Msg::RdSent(id, res) => match res {
                Ok(rid) => {
                    self.retry_at.remove(&id);
                    if let Some(g) = self.lib.get_mut(&id) {
                        g.rd_id = Some(rid);
                        self.status = format!("on Real-Debrid: {}", g.name);
                    }
                    self.remote.insert(id, Remote::Pending { state: "RD queued".into(), progress: 0.0 });
                    self.save_lib();
                    self.rd_last_poll = None;
                }
                Err(e) => {
                    self.status = format!("Real-Debrid: {e:#} (will retry in 5 min)");
                    self.retry_at.insert(id.clone(), Instant::now() + RETRY_AFTER);
                    self.remote.insert(id, Remote::Error(format!("Real-Debrid: {e:#}")));
                }
            },
            Msg::CacheSent(d, h, res) => match res {
                Ok(_) => {
                    let name = self.minerva_name(&h).unwrap_or_else(|| h.clone());
                    self.status = format!("{} is caching {name}; the Sources tab shows its progress", d.name());
                    self.cache_jobs.insert((d, h), CacheJob { state: "queued".into(), progress: 0.0, done: false });
                    match d {
                        Debrid::TorBox => self.last_poll = None,
                        Debrid::RealDebrid => self.rd_last_poll = None,
                    }
                }
                Err(e) => {
                    self.cache_jobs.remove(&(d, h));
                    self.status = format!("{} couldn't take the torrent: {e:#}", d.name());
                }
            },
            Msg::RdList(res) => {
                self.rd_polls_in_flight = 0;
                let items = match res {
                    Ok(items) => items,
                    Err(e) => {
                        self.status = format!("Real-Debrid status check failed: {e:#}");
                        return;
                    }
                };
                for ((_, h), job) in self.cache_jobs.iter_mut().filter(|((d, _), j)| *d == Debrid::RealDebrid && !j.done) {
                    // the whole-torrent copy: the one with the most progress for this hash
                    if let Some(i) =
                        items.iter().filter(|i| i.hash.eq_ignore_ascii_case(h)).max_by(|a, b| a.progress.total_cmp(&b.progress))
                    {
                        job.done = i.is_ready();
                        job.state = if job.done { "cached".into() } else { i.status.clone() };
                        job.progress = i.progress / 100.0;
                    }
                }
                let by_id: HashMap<&str, &crate::realdebrid::ListItem> = items.iter().map(|i| (i.id.as_str(), i)).collect();
                let mut gone = Vec::new();
                for g in self.lib.games.iter().filter(|g| !self.present.contains(&g.id)) {
                    let Some(rid) = g.rd_id.as_deref() else { continue };
                    if matches!(self.local.get(&g.id), Some(LocalState::Downloading { .. })) {
                        continue;
                    }
                    let r = match by_id.get(rid) {
                        Some(i) if i.is_ready() => Remote::Ready,
                        Some(i) if i.is_failed() => {
                            self.retry_at.insert(g.id.clone(), Instant::now() + RETRY_AFTER);
                            Remote::Error(format!("Real-Debrid: {}", i.status))
                        }
                        Some(i) => {
                            let peers = i.seeders.map(|s| format!(" · {s} seeders")).unwrap_or_default();
                            let speed = i.speed.filter(|s| *s > 0).map(|s| format!(" · {}/s", crate::ui::human(s))).unwrap_or_default();
                            Remote::Pending { state: format!("RD {}{peers}{speed}", i.status), progress: i.progress / 100.0 }
                        }
                        // An empty list is more likely a hiccup than every game deleted.
                        None if items.is_empty() => continue,
                        None => {
                            gone.push(g.id.clone());
                            Remote::Missing
                        }
                    };
                    self.remote.insert(g.id.clone(), r);
                }
                // Deleted on Real-Debrid: forget it so it gets added again.
                if !gone.is_empty() {
                    for id in gone {
                        if let Some(g) = self.lib.get_mut(&id) {
                            g.rd_id = None;
                        }
                    }
                    self.save_lib();
                }
            }
            Msg::MameTitles(res) => {
                self.mame_pending = false;
                match res {
                    Ok(t) => {
                        self.mame_titles = Arc::new(t);
                        self.mame_gen += 1;
                        // only MAME sets show titles; the rest keep their search text
                        let mame: Vec<String> =
                            self.sets.iter().filter(|s| crate::mame::is_mame_system(&s.system)).map(|s| s.item.clone()).collect();
                        for item in mame {
                            self.hay.remove(&item);
                        }
                    }
                    Err(e) => self.status = format!("MAME titles: {e:#} (search MAME by short name, e.g. sf2)"),
                }
            }
            Msg::MinervaCache(asked, res) => {
                self.cache_check_running = false;
                match res {
                    Ok(found) => {
                        for h in asked {
                            let names: HashSet<String> = self
                                .index
                                .values()
                                .find(|i| i.btih.as_deref() == Some(h.as_str()))
                                .map(|i| i.files.iter().map(|f| f.name.rsplit('/').next().unwrap_or(&f.name).to_lowercase()).collect())
                                .unwrap_or_default();
                            let files = found.get(&h).cloned().unwrap_or_default();
                            self.minerva_cache.insert(h, TorrentCache { files: (!fully_cached(&names, &files)).then_some(files) });
                        }
                        if self.cache_checked == 0 {
                            self.cache_checked = now_secs();
                        }
                        let file = CacheStatusFile { checked: self.cache_checked, torrents: self.minerva_cache.clone() };
                        if let Ok(json) = serde_json::to_vec(&file) {
                            let _ = std::fs::write(cache_status_path(&self.cache_dir), json);
                        }
                        let stuck: Vec<String> = self
                            .lib
                            .games
                            .iter()
                            .filter(|g| !self.present.contains(&g.id))
                            .filter(|g| matches!((&g.pick, g.btih()), (Some(p), Some(h)) if !self.minerva_available(&h, p)))
                            .map(|g| g.id.clone())
                            .collect();
                        for id in stuck {
                            if !matches!(self.local.get(&id), Some(LocalState::Downloading { .. })) {
                                self.send_to_torbox(&id);
                            }
                        }
                        self.refresh_local();
                    }
                    Err(e) => self.status = format!("checking TorBox's cache failed: {e:#}"),
                }
            }
            Msg::TorboxZips(res) => match res {
                Ok((cands, held, pending)) => {
                    self.tb_held = held;
                    // set torrents TorBox is still caching (sent from the Sources tab earlier)
                    let sets: HashSet<String> = self.index.values().filter_map(|i| i.btih.as_deref()).map(str::to_lowercase).collect();
                    for (h, (state, progress)) in pending.into_iter().filter(|(h, _)| sets.contains(h)) {
                        self.cache_jobs.insert((Debrid::TorBox, h), CacheJob { state, progress, done: false });
                    }
                    if let Ok(json) = serde_json::to_vec(&cands) {
                        let _ = std::fs::create_dir_all(&self.cache_dir);
                        let _ = std::fs::write(tb_zips_path(&self.cache_dir), json);
                    }
                    // torrents removed from TorBox drop out of search
                    let keep: HashSet<String> = cands.iter().map(tbzips::Candidate::key).collect();
                    self.sets.retain(|s| tbzips::parse_key(&s.item).is_none() || keep.contains(&s.item));
                    self.tb_zips = cands.iter().map(|c| (c.hash.clone(), c.clone())).collect();
                    let new: Vec<SetRef> =
                        tb_zip_sets(&cands).into_iter().filter(|s| !self.sets.iter().any(|x| x.item == s.item)).collect();
                    let pos = tb_zip_pos(&self.sets);
                    self.sets.splice(pos..pos, new);
                    self.apply_groups_off();
                    let to_index: Vec<String> = cands
                        .iter()
                        .map(tbzips::Candidate::key)
                        .filter(|k| self.index.get(k).is_none_or(|i| i.age() > REINDEX_AFTER))
                        .collect();
                    for item in to_index {
                        self.start_index(item);
                    }
                }
                Err(e) => self.status = format!("listing TorBox torrents failed: {e:#}"),
            },
            Msg::MinervaFolders(group, res) => {
                self.minerva_pending = self.minerva_pending.saturating_sub(1);
                match res {
                    Ok(folders) => {
                        save_folders(&self.cache_dir, &group, &folders);
                        let new: Vec<SetRef> =
                            minerva_sets(&group, &folders).into_iter().filter(|s| !self.sets.iter().any(|x| x.item == s.item)).collect();
                        let to_index: Vec<String> =
                            new.iter().filter(|s| !self.index.contains_key(&s.item)).map(|s| s.item.clone()).collect();
                        // keep Minerva ahead of the archive.org sets
                        let pos = self.sets.iter().take_while(|s| s.item.starts_with("minerva:")).count();
                        self.sets.splice(pos..pos, new);
                        self.apply_groups_off();
                        for item in to_index {
                            self.start_index(item);
                        }
                    }
                    Err(e) => self.status = format!("Minerva {group}: {e:#}"),
                }
            }
            Msg::Live(generation, res) => {
                if generation != self.live_gen {
                    return; // stale
                }
                self.live_loading = false;
                match res {
                    Ok(items) => {
                        // Remember restricted sets, so their games download directly.
                        for (h, idx) in items.iter().filter(|(_, i)| i.restricted) {
                            let _ = archive::save_cached(&self.cache_dir, &h.identifier, idx);
                            self.index.entry(h.identifier.clone()).or_insert_with(|| Arc::new(idx.clone()));
                        }
                        self.live_hits = self.live_to_hits(items);
                        self.merge_hits();
                    }
                    Err(e) => self.status = format!("archive.org search failed: {e:#}"),
                }
            }
            Msg::Vimm(generation, res) => {
                if generation != self.live_gen {
                    return;
                }
                match res {
                    Ok(hits) => {
                        let q = self.query();
                        self.vimm_hits = hits
                            .into_iter()
                            .filter(|h| q.matches(&format!("{} vimm", h.name().to_lowercase()), h.system))
                            .map(|h| RomHit {
                                url: h.url(),
                                name: h.name(),
                                size: 0,
                                system: h.system.to_string(),
                                source: "vimm.net (browser download)".into(),
                                live: true,
                                torrent: None,
                                title: None,
                            })
                            .collect();
                        self.merge_hits();
                    }
                    Err(e) => self.status = format!("Vimm search failed: {e:#}"),
                }
            }
            Msg::Sent(id, res) => match res {
                Ok(tid) => {
                    let hash = self.lib.get(&id).and_then(Game::btih);
                    // this game, plus any waiting games from the same torrent
                    let ids: Vec<String> = self
                        .lib
                        .games
                        .iter()
                        .filter(|g| g.id == id || (hash.is_some() && g.btih() == hash && g.torbox_id.is_none()))
                        .map(|g| g.id.clone())
                        .collect();
                    for gid in ids {
                        self.retry_at.remove(&gid);
                        if let Some(g) = self.lib.get_mut(&gid) {
                            g.torbox_id = Some(tid);
                            self.status = format!("on TorBox: {}", g.name);
                        }
                        self.remote.insert(gid, Remote::Pending { state: "queued".into(), progress: 0.0 });
                    }
                    self.save_lib();
                    self.last_poll = None; // poll soon
                }
                Err(e) => {
                    self.status = format!("TorBox: {e:#} (will retry in 5 min)");
                    self.retry_at.insert(id.clone(), Instant::now() + RETRY_AFTER);
                    self.remote.insert(id, Remote::Error(format!("{e:#}")));
                }
            },
            Msg::Remote(kind, res) => {
                self.polls_in_flight = self.polls_in_flight.saturating_sub(1);
                if let (Kind::Torrent, Ok(items)) = (kind, &res)
                    && !items.is_empty()
                {
                    self.tb_held = tbzips::held(items);
                    let pending = tbzips::pending(items);
                    for ((_, h), job) in self.cache_jobs.iter_mut().filter(|((d, _), _)| *d == Debrid::TorBox) {
                        match pending.get(h) {
                            Some((state, progress)) => (job.state, job.progress) = (state.clone(), *progress),
                            None if self.tb_held.contains_key(h) => (job.state, job.progress, job.done) = ("cached".into(), 1.0, true),
                            None => {}
                        }
                    }
                }
                match res {
                    // An empty list while games are waiting is almost always a TorBox hiccup;
                    // marking everything "gone" would make every game re-send.
                    Ok(items) if items.is_empty() && self.lib.games.iter().any(|g| g.torbox_id.is_some() && g.torbox_kind() == kind) => {
                        self.status = "TorBox returned an empty list; will check again".into();
                    }
                    Ok(items) => {
                        let by_id: HashMap<u64, &Item> = items.iter().map(|i| (i.id, i)).collect();
                        let mut dead: HashSet<u64> = HashSet::new();
                        let mut to_rd: Vec<String> = Vec::new();
                        for g in self.lib.games.iter().filter(|g| g.torbox_kind() == kind) {
                            let Some(tid) = g.torbox_id else { continue };
                            if matches!(self.remote.get(&g.id), Some(Remote::Sending)) {
                                continue;
                            }
                            let r = match by_id.get(&tid) {
                                Some(i)
                                    if (i.download_present || i.download_finished)
                                        && self.ctx.rd.is_some()
                                        && g.pick.as_deref().is_some_and(|p| !i.has_file(p, g.torrent_index)) =>
                                {
                                    // TorBox has only part of this torrent; Real-Debrid can get this file.
                                    to_rd.push(g.id.clone());
                                    Remote::Sending
                                }
                                Some(i)
                                    if (i.download_present || i.download_finished)
                                        && g.pick.as_deref().is_some_and(|p| !i.has_file(p, g.torrent_index)) =>
                                {
                                    // Don't re-send every poll: the cache won't change soon.
                                    self.retry_at.insert(g.id.clone(), Instant::now() + PARTIAL_RETRY);
                                    Remote::Error(format!(
                                        "not cached: TorBox has only {} file(s) of this torrent, not this game",
                                        i.real_files()
                                    ))
                                }
                                Some(i) if (i.download_present || i.download_finished) && !i.files.is_empty() => Remote::Ready,
                                Some(i) => Remote::Pending {
                                    state: if i.download_state.is_empty() { "pending".into() } else { i.download_state.clone() },
                                    progress: i.progress,
                                },
                                None => {
                                    dead.insert(tid);
                                    Remote::Missing
                                }
                            };
                            self.remote.insert(g.id.clone(), r);
                        }
                        if !to_rd.is_empty() {
                            self.save_lib();
                        }
                        for id in to_rd {
                            if let Some(g) = self.lib.get_mut(&id) {
                                g.torbox_id = None;
                            }
                            self.remote.remove(&id);
                            self.retry_at.remove(&id);
                            self.send_to_rd(&id);
                            self.save_lib();
                        }
                        // Forget ids TorBox no longer has, for every game sharing them, so a
                        // re-send asks TorBox again instead of reusing a sibling's dead id.
                        if !dead.is_empty() {
                            for g in self.lib.games.iter_mut().filter(|g| g.torbox_kind() == kind) {
                                if g.torbox_id.is_some_and(|t| dead.contains(&t)) {
                                    g.torbox_id = None;
                                    self.remote.insert(g.id.clone(), Remote::Missing);
                                }
                            }
                            self.save_lib();
                        }
                    }
                    Err(e) => self.status = format!("TorBox status check failed: {e:#}"),
                }
            }
            Msg::Scanned(found) => {
                self.scanning = false;
                self.sync_with_disk(found);
            }
            Msg::TexCatalog(r) => self.on_tex_catalog(r),
            Msg::Tex(key, state) => self.on_tex(key, state),
            Msg::Hay(item, idx, made, hay) => self.on_hay(item, &idx, made, hay),
            Msg::HaysDone => {
                self.hay_worker = false;
                if std::mem::take(&mut self.hay_fresh) && !self.rom_query.trim().is_empty() {
                    self.refresh_local();
                }
            }
            Msg::KeyChecked(field, res) => self.on_key_checked(field, res),
            Msg::ArchiveLogin(res) => self.on_archive_login(res),
            Msg::Local(id, state) => match state {
                LocalState::Done(files) => {
                    self.local.remove(&id);
                    if let Some(g) = self.lib.get_mut(&id) {
                        if g.system.is_empty()
                            && let Some((sys, _)) = files.first().and_then(|f| f.split_once('/'))
                        {
                            g.system = sys.to_string();
                        }
                        self.status = format!("downloaded {} → {}", g.name, files.join(", "));
                        g.local_files = files;
                    }
                    self.save_lib();
                    self.refresh_presence();
                }
                LocalState::Failed(ref m) => {
                    if let Some(g) = self.lib.get(&id) {
                        self.status = format!("download failed for {}: {m}", g.name);
                    }
                    let broken = m.contains(jobs::BROKEN_TORBOX_ZIP);
                    let btih = self.lib.get(&id).and_then(Game::btih);
                    self.local.insert(id, state);
                    if broken && let Some(h) = btih {
                        self.torbox_copy_broken(&h);
                    }
                }
                LocalState::Downloading { .. } => {
                    self.local.insert(id, state);
                }
            },
        }
    }

    // ---------- search ----------

    fn query(&self) -> systems::Query {
        systems::Query::parse(&self.rom_query)
    }

    /// Instant search over the indexed sets. Matches the file name, the set's title,
    /// and the system (e.g. "zelda gba", "god of war ps3", "sony crash", "redump europe").
    /// Sets that are searchable but have no search text yet.
    fn hays_missing(&self) -> Vec<usize> {
        (0..self.sets.len())
            .filter(|&i| {
                let item = &self.sets[i].item;
                !self.hay.contains_key(item) && self.index.get(item).is_some_and(|x| !self.hidden(x))
            })
            .collect()
    }

    /// Build the missing search text on a background thread (big sets take a while: the UI
    /// mustn't wait). Search skips sets without it and refreshes as it arrives.
    fn start_hays(&mut self) {
        if self.hay_worker {
            return;
        }
        let jobs: Vec<(SetRef, Arc<ItemIndex>)> =
            self.hays_missing().into_iter().map(|i| (self.sets[i].clone(), self.index[&self.sets[i].item].clone())).collect();
        if jobs.is_empty() {
            return;
        }
        self.hay_worker = true;
        let (titles, made, tx) = (self.mame_titles.clone(), self.mame_gen, self.tx.clone());
        tokio::task::spawn_blocking(move || {
            for (s, idx) in jobs {
                let hay = build_hay(&s, &idx, &titles);
                if tx.send(Msg::Hay(s.item, idx, made, hay)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::HaysDone);
        });
    }

    fn on_hay(&mut self, item: String, idx: &Arc<ItemIndex>, made: u64, hay: SetHay) {
        let current = self.index.get(&item).is_some_and(|i| Arc::ptr_eq(i, idx));
        let stale_titles = made != self.mame_gen && self.sets.iter().any(|s| s.item == item && crate::mame::is_mame_system(&s.system));
        if current && !stale_titles {
            self.hay.insert(item, hay);
            self.hay_fresh = true;
        }
    }

    /// Build every missing set's search text now, on this thread (headless commands, tests).
    pub fn build_hays_now(&mut self) {
        for i in self.hays_missing() {
            let h = build_hay(&self.sets[i], &self.index[&self.sets[i].item], &self.mame_titles);
            self.hay.insert(self.sets[i].item.clone(), h);
        }
    }

    #[cfg(test)]
    fn put_index(&mut self, item: String, idx: ItemIndex) {
        self.hay.remove(&item);
        self.index.insert(item, Arc::new(idx));
        self.build_hays_now();
    }

    /// Sets Search can't look through yet (their search text is being built).
    pub fn hays_pending(&self) -> usize {
        self.hays_missing().len()
    }

    pub fn refresh_local(&mut self) {
        let q = self.query();
        self.local_hits.clear();
        if !q.is_empty() {
            // Which files match, set by set, on every core (the scan is most of a keystroke);
            // then the hits in set order, here.
            let sets: Vec<(&SetRef, &ItemIndex, &SetHay)> = self
                .sets
                .iter()
                .filter(|s| self.cfg.source_on(&s.item))
                .filter_map(|s| {
                    let idx = self.index.get(&s.item).filter(|x| !self.hidden(x))?;
                    Some((s, &**idx, self.hay.get(&s.item)?))
                })
                .collect();
            let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get).min(8);
            let next = std::sync::atomic::AtomicUsize::new(0);
            // Hits in finished sets: sets are claimed in order, so once there are enough,
            // sets not started yet can't make the first MAX_LOCAL_HITS.
            let total = std::sync::atomic::AtomicUsize::new(0);
            let this = &*self;
            let mut found: Vec<Vec<u32>> = vec![Vec::new(); sets.len()];
            std::thread::scope(|scope| {
                let (q, sets, next, total) = (&q, &sets, &next, &total);
                let handles: Vec<_> = (0..threads)
                    .map(|_| {
                        scope.spawn(move || {
                            let mut out = Vec::new();
                            loop {
                                if total.load(std::sync::atomic::Ordering::Relaxed) >= MAX_LOCAL_HITS {
                                    return out;
                                }
                                let n = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let Some((_, idx, hay)) = sets.get(n) else { return out };
                                let sq = q.for_set(&hay.set_text);
                                // not on TorBox: let another source's copy show instead
                                let keep = this.minerva_keep(idx);
                                let gone = |i: usize| match keep {
                                    MinervaKeep::All => false,
                                    MinervaKeep::None => true,
                                    MinervaKeep::Cached(files) => {
                                        let name = &idx.files[i].name;
                                        !files.contains(&name.rsplit('/').next().unwrap_or(name).to_lowercase())
                                    }
                                };
                                let files: Vec<u32> = hay
                                    .files
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, (text, system, label, _))| sq.matches(text, label, system) && !gone(*i))
                                    .map(|(i, _)| i as u32)
                                    .take(MAX_LOCAL_HITS)
                                    .collect();
                                total.fetch_add(files.len(), std::sync::atomic::Ordering::Relaxed);
                                out.push((n, files));
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    for (n, files) in h.join().unwrap_or_default() {
                        found[n] = files;
                    }
                }
            });
            let mut hits = Vec::new();
            'outer: for ((s, idx, hay), files) in sets.iter().zip(found) {
                for i in files {
                    let (f, (_, system, _, title)) = (&idx.files[i as usize], &hay.files[i as usize]);
                    let source = if idx.title.is_empty() { s.item.clone() } else { idx.title.clone() };
                    let (url, torrent) = match &idx.btih {
                        Some(h) => (format!("minerva:{h}/{}", f.name), Some((h.clone(), f.torrent_index))),
                        None => (f.url(&s.item), None),
                    };
                    hits.push(RomHit {
                        url,
                        name: f.name.clone(),
                        size: f.size,
                        system: system.to_string(),
                        source,
                        live: false,
                        torrent,
                        title: title.clone(),
                    });
                    if hits.len() >= MAX_LOCAL_HITS {
                        break 'outer;
                    }
                }
            }
            self.local_hits = hits;
        }
        self.merge_hits();
    }

    fn live_to_hits(&self, items: Vec<(ItemHit, ItemIndex)>) -> Vec<RomHit> {
        let q = self.query();
        let mut out = Vec::new();
        for (hit, idx) in items {
            if hit.identifier.starts_with("thingiverse-") {
                continue; // 3D-printing models
            }
            let item_system = idx.system.clone().or_else(|| systems::guess_from_title(&hit.title).map(str::to_string)).unwrap_or_default();
            let title = format!("{} {}", hit.title, hit.identifier).to_lowercase();
            let sys_of = |name: &str| systems::guess_for_file(name).map(str::to_string).unwrap_or_else(|| item_system.clone());
            let mut files: Vec<_> =
                idx.files.iter().filter(|f| q.matches(&format!("{} {title}", f.name.to_lowercase()), &sys_of(&f.name))).collect();
            // Single-game uploads often have odd file names: take them whole if the item matches.
            if files.is_empty() && idx.files.len() <= 5 && q.matches(&title, &item_system) {
                files = idx.files.iter().collect();
            }
            for f in files {
                if !plausible_live_rom(&f.name, f.size, &sys_of(&f.name)) {
                    continue;
                }
                out.push(RomHit {
                    url: f.url(&hit.identifier),
                    name: f.name.clone(),
                    size: f.size,
                    system: sys_of(&f.name),
                    source: format!("archive.org/{}", hit.identifier),
                    live: true,
                    torrent: None,
                    title: None,
                });
            }
        }
        out
    }

    /// TorBox has this file cached right now, so it downloads straight away (⚡): a
    /// torrent in the TorBox account, or a Minerva file TorBox says it has.
    pub fn instant(&self, h: &RomHit) -> bool {
        let Some((btih, _)) = &h.torrent else { return false };
        let btih = btih.to_lowercase();
        if self.broken_torbox.contains_key(&btih) {
            return false;
        }
        if self.tb_zips.contains_key(&btih) {
            return true;
        }
        let base = h.name.rsplit('/').next().unwrap_or(&h.name).to_lowercase();
        match self.tb_held.get(&btih) {
            Some(None) => return true,
            Some(Some(files)) if files.contains(&base) => return true,
            _ => {}
        }
        matches!(self.minerva_cache.get(&btih), Some(TorrentCache { files: Some(f) }) if f.contains(&base))
    }

    /// Lower is a better source: cached on TorBox, then indexed sets, live archive.org, Vimm.
    fn source_rank(&self, h: &RomHit) -> u8 {
        if self.instant(h) {
            0
        } else if vimm::is_vimm_url(&h.url) {
            3
        } else if h.live {
            2
        } else {
            1
        }
    }

    /// The key of a search row, for keeping the selection across refreshes.
    fn merge_hits(&mut self) {
        let keep = self.rom_state.selected().and_then(|i| self.search_keys.get(i)).cloned();
        let mut seen = HashSet::new();
        let hits: Vec<RomHit> = self
            .local_hits
            .iter()
            .chain(self.live_hits.iter())
            .chain(self.vimm_hits.iter())
            .filter(|h| seen.insert(h.url.clone()))
            .cloned()
            .collect();
        // Same game from several sources shows once, best source first.
        if self.names.len() > 100_000 {
            self.names.clear();
        }
        let names = &mut self.names;
        let parsed: Vec<Arc<NameInfo>> = hits
            .iter()
            .map(|h| {
                names
                    .entry(format!("{}\0{}", h.system, h.name))
                    .or_insert_with(|| {
                        let g = crate::names::parse(&h.system, &h.name);
                        Arc::new(NameInfo { key: g.key(&h.system), display: g.display(), preference: crate::names::preference(&h.name) })
                    })
                    .clone()
            })
            .collect();
        let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, p) in parsed.iter().enumerate() {
            groups.entry(&p.key).or_default().push(i);
        }
        let base = |h: &RomHit| h.name.rsplit('/').next().unwrap_or(&h.name).to_string();
        let mut games: Vec<(String, Vec<usize>)> = groups.into_iter().map(|(k, g)| (k.to_string(), g)).collect();
        for (_, g) in &mut games {
            // cached on TorBox first, then the best version (finished game, USA, latest rev),
            // then the best source of it
            g.sort_by_cached_key(|&i| {
                let rank = self.source_rank(&hits[i]);
                (rank != 0, parsed[i].preference.clone(), rank, base(&hits[i]).len())
            });
        }
        // Grouped by system (Library order, undetected last), then alphabetical by name.
        let order = |s: &str| systems::SYSTEMS.iter().position(|(f, _)| *f == s).unwrap_or(usize::MAX);
        games.sort_by_cached_key(|(_, g)| {
            let h = &hits[g[0]];
            let name = h.title.clone().unwrap_or_else(|| parsed[g[0]].display.clone());
            (order(&h.system), h.system.clone(), name.to_lowercase())
        });
        let (mut rows, mut keys) = (Vec::new(), Vec::new());
        let mut i = 0;
        while i < games.len() {
            let system = hits[games[i].1[0]].system.clone();
            let n = games[i..].iter().take_while(|(_, g)| hits[g[0]].system == system).count();
            let collapsed = self.search_collapsed.contains(&system);
            keys.push(format!("\u{1}{system}"));
            rows.push(SearchRow::Header { system, count: n, collapsed });
            if !collapsed {
                for (k, g) in &games[i..i + n] {
                    keys.push(k.clone());
                    rows.push(SearchRow::Game(g.clone()));
                }
            }
            i += n;
        }
        self.rom_hits = hits;
        self.search_rows = rows;
        self.search_keys = keys;
        let sel = keep.and_then(|k| self.search_keys.iter().position(|x| *x == k));
        // start on the first game, not its header
        let first =
            self.search_rows.iter().position(|r| matches!(r, SearchRow::Game(_))).or_else(|| (!self.search_rows.is_empty()).then_some(0));
        self.rom_state.select(sel.or(first));
    }

    /// The selected search row's hits, best first (None on a header).
    pub fn selected_hits(&self) -> Option<&[usize]> {
        match self.rom_state.selected().and_then(|i| self.search_rows.get(i))? {
            SearchRow::Game(hits) => Some(hits),
            SearchRow::Header { .. } => None,
        }
    }

    fn selected_hit(&self) -> Option<&RomHit> {
        self.selected_hits().and_then(|h| self.rom_hits.get(h[0]))
    }

    /// The system of the selected search row (a header's own, or its game's).
    fn search_cursor_system(&self) -> Option<String> {
        match self.rom_state.selected().and_then(|i| self.search_rows.get(i))? {
            SearchRow::Header { system, .. } => Some(system.clone()),
            SearchRow::Game(hits) => self.rom_hits.get(hits[0]).map(|h| h.system.clone()),
        }
    }

    fn set_search_collapsed(&mut self, system: &str, collapsed: bool) {
        if collapsed {
            self.search_collapsed.insert(system.to_string());
        } else {
            self.search_collapsed.remove(system);
        }
        self.merge_hits();
        // keep the cursor on the folded group's header
        if collapsed && let Some(i) = self.search_rows.iter().position(|r| matches!(r, SearchRow::Header { system: s, .. } if s == system))
        {
            self.rom_state.select(Some(i));
        }
    }

    /// Enter in search results: fold a header; add a game, or pick among its sources.
    fn activate_search_row(&mut self) {
        if let Some(sys) = self.rom_state.selected().and_then(|i| self.search_rows.get(i)).and_then(|r| match r {
            SearchRow::Header { system, collapsed, .. } => Some((system.clone(), *collapsed)),
            SearchRow::Game(_) => None,
        }) {
            return self.set_search_collapsed(&sys.0, !sys.1);
        }
        let Some(hits) = self.selected_hits() else { return };
        if hits.len() > 1 && !hits.iter().any(|&i| self.lib.contains(&self.rom_hits[i].url)) {
            return self.open_source_menu();
        }
        self.add_selected_rom();
    }

    /// The selected game's sources to choose from, best first.
    fn open_source_menu(&mut self) {
        let Some(hits) = self.selected_hits() else { return };
        let items = hits
            .iter()
            .map(|&i| {
                let h = &self.rom_hits[i];
                let bolt = if self.instant(h) { "⚡ " } else { "   " };
                let size = if h.size > 0 { crate::ui::human(h.size) } else { "size ?".into() };
                let base = h.name.rsplit('/').next().unwrap_or(&h.name);
                let source: String = h.source.chars().take(48).collect();
                (format!("{bolt}{size:>9}  {base}  · {source}"), Act::AddSource(i))
            })
            .collect();
        let h = &self.rom_hits[hits[0]];
        let name = h.title.clone().unwrap_or_else(|| h.name.rsplit('/').next().unwrap_or(&h.name).to_string());
        self.menu = Some(Menu { title: format!("{name} · pick a source"), items, sel: 0 });
    }

    /// Run a search immediately (local + live), for the headless `search` command.
    /// Just the instant (indexed) part of a search, as typing runs it.
    pub fn refresh_local_for(&mut self, query: &str) {
        self.rom_query = query.to_string();
        self.refresh_local();
    }

    pub fn search_now(&mut self, query: &str) {
        self.rom_query = query.to_string();
        self.on_query_change();
        self.live_due = None;
        self.start_live_search();
    }

    fn on_query_change(&mut self) {
        self.live_hits.clear();
        self.vimm_hits.clear();
        self.live_gen += 1; // drop any in-flight live results
        self.live_loading = false;
        self.refresh_local();
        self.live_due = (self.rom_query.trim().len() >= 3).then(|| Instant::now() + LIVE_DEBOUNCE);
    }

    /// Add the selected game from its best source.
    fn add_selected_rom(&mut self) {
        let Some(hits) = self.selected_hits() else { return };
        if hits.iter().any(|&i| self.lib.contains(&self.rom_hits[i].url)) {
            self.status = "already in your library".into();
            return;
        }
        self.add_hit(hits[0]);
    }

    /// Add one search hit (one source of a game) to the library.
    fn add_hit(&mut self, i: usize) {
        let Some(hit) = self.rom_hits.get(i) else { return };
        if self.lib.contains(&hit.url) {
            self.status = "already in your library".into();
            return;
        }
        if hit.system.is_empty() {
            self.status = "couldn't detect the system for this file; pick one with S first".into();
            return;
        }
        if self.on_disk.contains(&Self::disk_key(&hit.system, hit.name.rsplit('/').next().unwrap_or(&hit.name))) {
            self.status = "already in your roms folder".into();
            return;
        }
        let game = Self::game_from_hit(hit);
        self.add_game(game);
    }

    fn add_game(&mut self, game: Game) {
        let id = game.id.clone();
        if game.kind == SourceKind::Vimm {
            self.status = format!("added {} · Enter opens Vimm's page in a browser to download it", game.name);
            self.collapsed.remove(&game.system);
            self.lib.games.push(game);
            self.save_lib();
            self.lib_cursor = Some(RowKey::Game(id));
            return;
        }
        if self.direct_only(&game.source) {
            // IF Archive (free and small) or a restricted archive.org set TorBox can't
            // fetch: no TorBox round trip, just download it.
            let from = if ifarchive::is_url(&game.source) { "the IF Archive" } else { "archive.org" };
            self.status = format!("added {}, downloading from {from}…", self.game_name(&game));
            self.collapsed.remove(&game.system);
            self.remote.insert(id.clone(), Remote::Direct);
            self.lib.games.push(game.clone());
            self.save_lib();
            self.lib_cursor = Some(RowKey::Game(id));
            self.start_local(game, false);
            return;
        }
        self.status = format!(
            "added {} to library{}",
            game.name,
            if self.ctx.torbox.is_some() { ", sending to TorBox…" } else { " (no TorBox key: use d in Library to download directly)" }
        );
        self.lib.games.push(game);
        self.save_lib();
        self.remote.insert(id.clone(), if self.ctx.torbox.is_some() { Remote::NotSent } else { Remote::NoKey });
        if let Some(g) = self.lib.get(&id) {
            self.collapsed.remove(&g.system); // show where it went
        }
        self.lib_cursor = Some(RowKey::Game(id.clone()));
        self.send_to_torbox(&id);
    }

    // ---------- sources ----------

    /// Which group of the Sources tab a set belongs to.
    fn set_group(&self, item: &str) -> &'static str {
        if let Some((group, _)) = minerva::parse_key(item) {
            return if group == "Redump" { SET_GROUPS[1] } else { SET_GROUPS[0] };
        }
        if tbzips::parse_key(item).is_some() {
            return SET_GROUPS[2];
        }
        if item == ifarchive::KEY {
            return SET_GROUPS[6];
        }
        if self.cfg.extra_sets.iter().any(|c| c.item == item) {
            return SET_GROUPS[5];
        }
        if self.index.get(item).is_some_and(|i| i.restricted) { SET_GROUPS[4] } else { SET_GROUPS[3] }
    }

    pub fn set_info(&self, item: &str, system: &str) -> SetInfo {
        let idx = self.index.get(item);
        let title = match (idx.map(|i| i.title.as_str()).filter(|t| !t.is_empty()), minerva::parse_key(item)) {
            (_, Some((_, folder))) => folder.to_string(),
            (Some(t), None) => t.to_string(),
            (None, None) => item.to_string(),
        };
        let state = if idx.is_some_and(|i| self.hidden(i)) {
            SetState::Locked
        } else if self.indexing.contains(item) {
            SetState::Indexing
        } else if idx.is_some() {
            SetState::Indexed
        } else if let Some(e) = self.index_errors.get(item) {
            SetState::Failed(e.clone())
        } else {
            SetState::Waiting
        };
        let cached = tbzips::parse_key(item).is_some()
            || idx.and_then(|i| i.btih.as_deref()).is_some_and(|h| matches!(self.tb_held.get(&h.to_lowercase()), Some(None)));
        let system = if system.is_empty() { idx.and_then(|i| i.system.clone()).unwrap_or_default() } else { system.to_string() };
        let h = idx.and_then(|i| i.btih.as_deref()).map(str::to_lowercase).unwrap_or_default();
        let caching = [Debrid::TorBox, Debrid::RealDebrid]
            .into_iter()
            .filter_map(|d| self.cache_jobs.get(&(d, h.clone())).map(|j| (d.name(), j.state.clone(), j.progress, j.done)))
            .collect();
        SetInfo {
            caching,
            on: self.cfg.source_on(item),
            title,
            system,
            files: idx.map_or(0, |i| i.files.len()),
            size: idx.map_or(0, |i| i.files.iter().map(|f| f.size).sum()),
            state,
            cached,
            age: idx.filter(|i| i.fetched > 0).map(|i| i.age().as_secs()),
        }
    }

    /// Group headers, and each open group's sets (by system, then title).
    pub fn set_rows(&self) -> Vec<SetRow> {
        let order = |s: &str| systems::SYSTEMS.iter().position(|(f, _)| *f == s).unwrap_or(usize::MAX);
        let mut rows = Vec::new();
        for name in SET_GROUPS {
            if *name == LIVE_GROUP {
                let open = self.sets_open.contains(LIVE_GROUP);
                let off = LIVE_SOURCES.iter().filter(|(k, _)| !self.cfg.source_on(k)).count();
                rows.push(SetRow::Group { name, sets: LIVE_SOURCES.len(), off, files: 0, collapsed: !open });
                if open {
                    rows.extend(LIVE_SOURCES.iter().map(|(k, _)| SetRow::Live(k)));
                }
                continue;
            }
            let mut sets: Vec<(&SetRef, String)> = self
                .sets
                .iter()
                .filter(|s| self.set_group(&s.item) == *name)
                .map(|s| (s, self.set_info(&s.item, &s.system).title.to_lowercase()))
                .collect();
            if sets.is_empty() {
                continue;
            }
            sets.sort_by(|a, b| (order(&a.0.system), &a.1).cmp(&(order(&b.0.system), &b.1)));
            let files = sets.iter().filter_map(|(s, _)| self.index.get(&s.item)).map(|i| i.files.len()).sum();
            let off = sets.iter().filter(|(s, _)| !self.cfg.source_on(&s.item)).count();
            let open = self.sets_open.contains(*name);
            rows.push(SetRow::Group { name, sets: sets.len(), off, files, collapsed: !open });
            if open {
                rows.extend(sets.into_iter().map(|(s, _)| SetRow::Set(s.item.clone())));
            }
        }
        rows
    }

    pub fn set_row_key(row: &SetRow) -> String {
        match row {
            SetRow::Group { name, .. } => format!("\u{1}{name}"),
            SetRow::Set(item) => item.clone(),
            SetRow::Live(key) => format!("\u{2}{key}"),
        }
    }

    /// The live-search source under the Sources cursor.
    fn selected_live(&self) -> Option<&'static str> {
        let key = self.sets_cursor.as_deref()?.strip_prefix('\u{2}')?;
        LIVE_SOURCES.iter().map(|(k, _)| *k).find(|k| *k == key)
    }

    /// Source keys a Sources row stands for: a set or site, or every one in a group.
    fn row_sources(&self) -> Vec<String> {
        if let Some(k) = self.selected_live() {
            return vec![k.to_string()];
        }
        if let Some(s) = self.selected_set() {
            return vec![s.item];
        }
        match self.sets_cursor_group() {
            Some(LIVE_GROUP) => LIVE_SOURCES.iter().map(|(k, _)| k.to_string()).collect(),
            Some(g) => self.sets.iter().filter(|s| self.set_group(&s.item) == g).map(|s| s.item.clone()).collect(),
            None => Vec::new(),
        }
    }

    /// Space / menu: switch the row's sources on or off (a group: all off if any is on,
    /// else all on). Saved to the config; search results follow at once.
    fn toggle_source_row(&mut self) {
        let keys = self.row_sources();
        if keys.is_empty() {
            return;
        }
        let on = !keys.iter().any(|k| self.cfg.source_on(k));
        for k in &keys {
            self.cfg.set_source(k, on);
        }
        let what = match keys.as_slice() {
            [k] => LIVE_SOURCES
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, t)| t.split(" (").next().unwrap_or(t).to_string())
                .unwrap_or_else(|| self.set_info(k, "").title),
            _ => format!("{} sources", keys.len()),
        };
        self.status = format!("{what} {}", if on { "on" } else { "off" });
        self.save_config();
        if on {
            let missing: Vec<String> =
                keys.iter().filter(|k| self.sets.iter().any(|s| &s.item == *k) && !self.index.contains_key(*k)).cloned().collect();
            for k in missing {
                self.start_index(k);
            }
        }
        if !self.rom_query.trim().is_empty() {
            self.on_query_change();
        }
    }

    /// The SetRef under the Sources cursor (None on a group header).
    fn selected_set(&self) -> Option<SetRef> {
        let c = self.sets_cursor.as_ref()?;
        self.sets.iter().find(|s| &s.item == c).cloned()
    }

    fn sets_cursor_group(&self) -> Option<&'static str> {
        let c = self.sets_cursor.as_ref()?;
        if c.starts_with('\u{2}') {
            return Some(LIVE_GROUP);
        }
        match c.strip_prefix('\u{1}') {
            Some(name) => SET_GROUPS.iter().find(|g| **g == name).copied(),
            None => Some(self.set_group(c)),
        }
    }

    fn set_group_open(&mut self, group: &'static str, open: bool) {
        if open {
            self.sets_open.insert(group.to_string());
        } else {
            self.sets_open.remove(group);
            self.sets_cursor = Some(format!("\u{1}{group}")); // don't leave the cursor in a closed group
        }
    }

    fn activate_set_row(&mut self) {
        match self.selected_set() {
            _ if self.selected_live().is_some() => self.set_item_menu(),
            Some(_) => self.set_item_menu(),
            None => {
                if let Some(g) = self.sets_cursor_group().or(SET_GROUPS.first().copied()) {
                    let open = self.sets_open.contains(g);
                    self.sets_cursor.get_or_insert_with(|| format!("\u{1}{g}"));
                    self.set_group_open(g, !open);
                }
            }
        }
    }

    fn set_item_menu(&mut self) {
        if let Some(k) = self.selected_live() {
            let title = LIVE_SOURCES.iter().find(|(key, _)| *key == k).map_or(k, |(_, t)| t);
            let toggle = if self.cfg.source_on(k) { "Turn off" } else { "Turn on" };
            self.menu =
                Some(Menu { title: title.into(), items: vec![(toggle.into(), Act::ToggleSource), ("Close".into(), Act::Close)], sel: 0 });
            return;
        }
        let Some(s) = self.selected_set() else {
            let Some(g) = self.sets_cursor_group() else { return };
            let open = self.sets_open.contains(g);
            let toggle = if self.row_sources().iter().any(|k| self.cfg.source_on(k)) { "Turn all off" } else { "Turn all on" };
            self.menu = Some(Menu {
                title: g.into(),
                items: vec![(if open { "Close" } else { "Open" }.into(), Act::Fold), (toggle.into(), Act::ToggleSource)],
                sel: 0,
            });
            return;
        };
        let info = self.set_info(&s.item, &s.system);
        let mut items = vec![(if info.on { "Turn off" } else { "Turn on" }.into(), Act::ToggleSource)];
        if !info.system.is_empty() && info.on {
            items.push((format!("Search {}", systems::label(&info.system)), Act::SetSearch));
        }
        if info.on {
            items.push((if self.indexing.contains(&s.item) { "Indexing…" } else { "Index again now" }.into(), Act::Reindex));
        }
        for d in self.cache_targets(&s.item) {
            items.push((format!("Cache to {}", d.name()), Act::CacheTo(d)));
        }
        items.push(("Close".into(), Act::Close));
        self.menu = Some(Menu { title: info.title, items, sel: 0 });
    }

    /// Jump to Search with the set's system as the filter, ready to type a title.
    fn search_set_system(&mut self) {
        let Some(s) = self.selected_set() else { return };
        let sys = self.set_info(&s.item, &s.system).system;
        self.set_tab(Tab::Search);
        self.rom_query = format!("{sys} ");
        self.editing = true;
        self.on_query_change();
    }

    /// Whether TorBox already has this whole torrent: in the account, or fully cached.
    fn torbox_has_all(&self, btih: &str, idx: &ItemIndex) -> bool {
        let h = btih.to_lowercase();
        let names = || idx.files.iter().map(|f| f.name.rsplit('/').next().unwrap_or(&f.name).to_lowercase()).collect::<HashSet<_>>();
        match self.tb_held.get(&h) {
            Some(None) => return true,
            Some(Some(files)) if fully_cached(&names(), files) => return true,
            _ => {}
        }
        matches!(self.minerva_cache.get(&h), Some(TorrentCache { files: None }))
    }

    /// Debrid services a set's torrent can be sent to for caching: torrent-backed sets
    /// under 1 TB that the service doesn't have whole and isn't caching already.
    fn cache_targets(&self, item: &str) -> Vec<Debrid> {
        let Some(idx) = self.index.get(item) else { return Vec::new() };
        let Some(h) = idx.btih.as_deref().filter(|_| tbzips::parse_key(item).is_none()) else { return Vec::new() };
        if idx.files.iter().map(|f| f.size).sum::<u64>() >= CACHE_MAX {
            return Vec::new();
        }
        let busy = |d: Debrid| self.cache_jobs.contains_key(&(d, h.to_lowercase()));
        let mut out = Vec::new();
        if self.ctx.torbox.is_some() && !busy(Debrid::TorBox) && !self.torbox_has_all(h, idx) {
            out.push(Debrid::TorBox);
        }
        if self.ctx.rd.is_some() && !busy(Debrid::RealDebrid) {
            out.push(Debrid::RealDebrid);
        }
        out
    }

    /// Send the selected set's whole torrent to a debrid service to cache.
    fn cache_selected_set(&mut self, d: Debrid) {
        let Some(s) = self.selected_set() else { return };
        let Some(h) = self.index.get(&s.item).and_then(|i| i.btih.clone()).map(|h| h.to_lowercase()) else { return };
        let magnet = minerva::magnet(&h);
        let name = self.minerva_name(&h);
        let tx = self.tx.clone();
        self.cache_jobs.insert((d, h.clone()), CacheJob { state: "sending".into(), progress: 0.0, done: false });
        self.status = format!("sending {} to {}…", self.set_info(&s.item, &s.system).title, d.name());
        match d {
            Debrid::TorBox => {
                let Some(tb) = self.ctx.torbox.clone() else { return };
                tokio::spawn(async move {
                    let res = tb.create(Kind::Torrent, &magnet, name.as_deref(), false).await.map(|id| id.to_string());
                    let _ = tx.send(Msg::CacheSent(d, h, res));
                });
            }
            Debrid::RealDebrid => {
                let Some(rd) = self.ctx.rd.clone() else { return };
                tokio::spawn(async move {
                    let res = rd.add_all(&magnet, &h).await;
                    let _ = tx.send(Msg::CacheSent(d, h, res));
                });
            }
        }
    }

    fn reindex_selected_set(&mut self) {
        let Some(s) = self.selected_set() else { return };
        if tbzips::parse_key(&s.item).is_some_and(|h| !self.tb_zips.contains_key(h)) {
            self.status = "that torrent is no longer on TorBox".into();
            return;
        }
        self.index_errors.remove(&s.item);
        self.start_index(s.item.clone());
        self.status = format!("indexing {} again…", self.set_info(&s.item, &s.system).title);
    }

    // ---------- library ----------

    /// Lower sorts first within a system: things that need attention before games on disk.
    fn attention(&self, g: &Game) -> u8 {
        if matches!(self.local.get(&g.id), Some(LocalState::Downloading { .. })) {
            return 0;
        }
        if self.needs_key.contains(&g.id) {
            return 1;
        }
        if self.present.contains(&g.id) {
            return 6;
        }
        if matches!(self.local.get(&g.id), Some(LocalState::Failed(_))) {
            return 3;
        }
        match self.remote.get(&g.id) {
            Some(Remote::Ready) => 1,
            Some(Remote::Sending | Remote::Pending { .. }) => 2,
            Some(Remote::Error(_) | Remote::Missing) => 3,
            _ if g.kind == SourceKind::Local => 5, // local entry whose file vanished
            _ if g.kind == SourceKind::Vimm && self.vimm_waiting.contains_key(&g.id) => 2,
            _ => 4,
        }
    }

    /// Library rows grouped by system (in SYSTEMS order, unknown last). Collapsed systems
    /// show only their header; open ones show up to LIB_GROUP_LIMIT games. A filter shows
    /// every match, ignoring both.
    pub fn lib_rows(&self) -> Vec<Row> {
        let terms: Vec<String> = self.lib_filter.to_lowercase().split_whitespace().map(String::from).collect();
        let filtering = !terms.is_empty();
        let mut idx: Vec<usize> = (0..self.lib.games.len())
            .filter(|&i| {
                let g = &self.lib.games[i];
                matches_all(&format!("{} {} {}", g.name, g.title.as_deref().unwrap_or(""), g.system), &terms)
            })
            .collect();
        let order = |s: &str| systems::SYSTEMS.iter().position(|(f, _)| *f == s).unwrap_or(usize::MAX);
        match self.lib_sort {
            LibSort::Name => idx.sort_by_cached_key(|&i| {
                let g = &self.lib.games[i];
                (order(&g.system), self.attention(g), g.name.to_lowercase())
            }),
            LibSort::Size => {
                // on disk first; what's not downloaded yet only breaks ties
                let mut totals: HashMap<&str, (u64, u64)> = HashMap::new();
                for &i in &idx {
                    let g = &self.lib.games[i];
                    let t = totals.entry(g.system.as_str()).or_default();
                    t.0 += self.size_on_disk(g);
                    t.1 += g.size;
                }
                idx.sort_by_cached_key(|&i| {
                    let g = &self.lib.games[i];
                    let total = totals.get(g.system.as_str()).copied().unwrap_or_default();
                    let size = (self.size_on_disk(g), g.size);
                    (std::cmp::Reverse(total), order(&g.system), std::cmp::Reverse(size), g.name.to_lowercase())
                });
            }
        }
        let mut rows = Vec::new();
        let mut i = 0;
        while i < idx.len() {
            let sys = self.lib.games[idx[i]].system.clone();
            let count = idx[i..].iter().take_while(|&&j| self.lib.games[j].system == sys).count();
            let collapsed = !filtering && self.collapsed.contains(&sys);
            rows.push(Row::Header { system: sys.clone(), count, collapsed });
            if !collapsed {
                let shown = if filtering { count } else { count.min(LIB_GROUP_LIMIT) };
                rows.extend(idx[i..i + shown].iter().map(|&j| Row::Game(j)));
                if shown < count {
                    rows.push(Row::More { system: sys, hidden: count - shown });
                }
            }
            i += count;
        }
        rows
    }

    pub fn row_key(&self, r: &Row) -> RowKey {
        match r {
            Row::Header { system, .. } => RowKey::Header(system.clone()),
            Row::Game(i) => RowKey::Game(self.lib.games[*i].id.clone()),
            Row::More { system, .. } => RowKey::More(system.clone()),
        }
    }

    fn row_keys(&self) -> Vec<RowKey> {
        self.lib_rows().iter().map(|r| self.row_key(r)).collect()
    }

    fn first_row(&self) -> Option<RowKey> {
        let keys = self.row_keys();
        keys.iter().find(|k| matches!(k, RowKey::Game(_))).or_else(|| keys.first()).cloned()
    }

    /// Keep the cursor on a visible row (e.g. after filtering, collapsing, removing).
    fn fix_cursor(&mut self) {
        let keys = self.row_keys();
        if !self.lib_cursor.as_ref().is_some_and(|c| keys.contains(c)) {
            // a hidden game: fall back to its system's header
            let header = match &self.lib_cursor {
                Some(RowKey::Game(id)) => self.lib.get(id).map(|g| RowKey::Header(g.system.clone())),
                Some(RowKey::More(sys)) => Some(RowKey::Header(sys.clone())),
                _ => None,
            };
            self.lib_cursor = header.filter(|h| keys.contains(h)).or_else(|| self.first_row());
        }
    }

    /// The system of whatever row the cursor is on.
    fn cursor_system(&self) -> Option<String> {
        match self.lib_cursor.as_ref()? {
            RowKey::Header(s) | RowKey::More(s) => Some(s.clone()),
            RowKey::Game(id) => self.lib.get(id).map(|g| g.system.clone()),
        }
    }

    fn maybe_auto_collapse(&mut self) {
        if !self.auto_collapsed && self.lib.games.len() > AUTO_COLLAPSE_OVER {
            self.collapsed = self.lib.games.iter().map(|g| g.system.clone()).collect();
            self.auto_collapsed = true;
        }
    }

    fn set_collapsed(&mut self, system: &str, collapse: bool) {
        if collapse {
            self.collapsed.insert(system.to_string());
            self.lib_cursor = Some(RowKey::Header(system.to_string()));
        } else {
            self.collapsed.remove(system);
        }
        self.fix_cursor();
    }

    fn toggle_current_group(&mut self) {
        if let Some(sys) = self.cursor_system() {
            let collapse = !self.collapsed.contains(&sys);
            self.set_collapsed(&sys, collapse);
        }
    }

    fn set_all_collapsed(&mut self, collapse: bool) {
        if collapse {
            if let Some(sys) = self.cursor_system() {
                self.lib_cursor = Some(RowKey::Header(sys));
            }
            self.collapsed = self.lib.games.iter().map(|g| g.system.clone()).collect();
        } else {
            self.collapsed.clear();
        }
        self.fix_cursor();
    }

    /// "… N more": show the whole system by filtering to it.
    fn show_all_of(&mut self, system: &str) {
        self.lib_filter = system.to_string();
        self.lib_input = LibInput::Filter;
        self.lib_cursor = None;
        self.fix_cursor();
        self.status = format!("showing every {system} game · clear the filter (/ then Backspace) to go back");
    }

    /// Enter in the Library: toggle a header, expand a "more" row, download a game.
    /// Enter / A: the row's default action.
    fn activate_row(&mut self) {
        match self.lib_cursor.clone() {
            Some(RowKey::Header(sys)) => {
                let collapse = !self.collapsed.contains(&sys);
                self.set_collapsed(&sys, collapse);
            }
            Some(RowKey::More(sys)) => self.show_all_of(&sys),
            Some(RowKey::Game(_)) => self.download_selected(false),
            None => {}
        }
    }

    fn selected_game(&self) -> Option<&Game> {
        match &self.lib_cursor {
            Some(RowKey::Game(id)) => self.lib.get(id),
            _ => None,
        }
    }

    fn download_selected(&mut self, direct: bool) {
        let Some(g) = self.selected_game().cloned() else { return };
        if self.needs_key.contains(&g.id) && !matches!(self.local.get(&g.id), Some(LocalState::Downloading { .. })) {
            // Already on disk: just unzip and/or fetch the disc key, no game download.
            self.status = format!("getting the disc key for {}…", g.name);
            self.start_local(g, true);
            return;
        }
        if self.present.contains(&g.id) {
            self.status = format!("already on disk: {}", g.local_paths(&self.ctx.roms_dir)[0].display());
            return;
        }
        if matches!(self.local.get(&g.id), Some(LocalState::Downloading { .. })) {
            self.status = "already downloading".into();
            return;
        }
        if g.kind == SourceKind::Vimm {
            return self.open_vimm(&g);
        }
        if g.kind == SourceKind::Local {
            self.status = "no longer in the roms folder; find it in Search to download it again".into();
            return;
        }
        if direct {
            if g.kind != SourceKind::Link {
                self.status = "magnets can only be downloaded through TorBox".into();
            } else if g.system.is_empty() {
                self.status = "pick a system with S first".into();
            } else {
                self.status = format!("downloading {} directly…", g.name);
                self.start_local(g, false);
            }
            return;
        }
        match self.remote.get(&g.id).cloned().unwrap_or(Remote::NotSent) {
            Remote::Ready => {
                self.status = format!("downloading {} from TorBox…", g.name);
                self.start_local(g, true);
            }
            Remote::Pending { state, progress } => {
                self.status = format!("TorBox is still fetching it ({state} {:.0}%); it'll show ☁ ready when done", progress * 100.0);
            }
            Remote::Sending => self.status = "still sending to TorBox…".into(),
            Remote::NotSent | Remote::Missing | Remote::Error(_) => {
                self.status = format!("sending {} to TorBox…", g.name);
                self.retry_at.remove(&g.id);
                self.send_to_torbox(&g.id);
            }
            Remote::Direct => {
                self.status = format!("downloading {}…", self.game_name(&g));
                self.start_local(g, false);
            }
            Remote::NoKey => {
                if g.kind == SourceKind::Link && !g.system.is_empty() {
                    self.status = format!("no TorBox key; downloading {} directly…", g.name);
                    self.start_local(g, false);
                } else {
                    self.status = "no TorBox key configured".into();
                }
            }
        }
    }

    fn add_pasted_link(&mut self) {
        let link = self.lib_paste.trim().to_string();
        self.lib_paste.clear();
        self.lib_input = LibInput::Filter;
        if link.is_empty() {
            return;
        }
        let kind = if link.starts_with("magnet:") {
            SourceKind::Magnet
        } else if link.starts_with("http://") || link.starts_with("https://") {
            SourceKind::Link
        } else {
            self.status = "expected a magnet: or http(s):// link".into();
            return;
        };
        if self.lib.contains(&link) {
            self.status = "already in your library".into();
            return;
        }
        if let Some(why) = minerva::btih_of(&link).and_then(|h| self.whole_minerva_folder(&h)) {
            self.status = why;
            return;
        }
        let system = systems::guess_from_link(&link).unwrap_or("").to_string();
        self.add_game(Game::new(library::name_from_link(&link), system, kind, link, 0));
    }

    /// A magnet for a whole Minerva system folder (tens of TB): TorBox can't fetch that,
    /// and it has no way to pick one file. Returns the message to show instead.
    fn whole_minerva_folder(&self, btih: &str) -> Option<String> {
        let idx = self.index.values().find(|i| i.btih.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(btih)))?;
        let size: u64 = idx.files.iter().map(|f| f.size).sum();
        let folder = idx.title.rsplit(" · ").next().unwrap_or(&idx.title);
        Some(format!(
            "that magnet is Minerva's whole '{folder}' folder ({}); TorBox can't download it. Find the game in Search instead",
            crate::ui::human(size)
        ))
    }

    fn remove_selected(&mut self) {
        let Some(id) = self.selected_game().map(|g| g.id.clone()) else { return };
        let before = self.row_keys();
        let pos = before.iter().position(|k| *k == RowKey::Game(id.clone())).unwrap_or(0);
        self.lib.games.retain(|g| g.id != id);
        self.download_queue.retain(|q| *q != id);
        self.remote.remove(&id);
        self.local.remove(&id);
        self.retry_at.remove(&id);
        self.save_lib();
        self.refresh_presence();
        self.status = "removed from library (local files and the TorBox copy were left alone)".into();
        let after = self.row_keys();
        self.lib_cursor = after.get(pos.min(after.len().saturating_sub(1))).cloned();
    }

    /// The files of a game that are on disk: its library files plus what belongs with
    /// them (a PS3 ISO's .dkey, leftover .part files). Only paths inside roms_dir/<system>/.
    fn files_on_disk(&self, g: &Game) -> Vec<PathBuf> {
        let roms = &self.ctx.roms_dir;
        let mut out: Vec<PathBuf> = Vec::new();
        for p in g.local_paths(roms) {
            let inside = p
                .strip_prefix(roms)
                .is_ok_and(|r| r.components().count() == 2 && r.components().all(|c| matches!(c, std::path::Component::Normal(_))));
            if !inside {
                continue;
            }
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut extra = vec![p.with_file_name(format!("{name}.part"))];
            if name.to_lowercase().ends_with(".iso") {
                extra.push(p.with_extension("dkey"));
            }
            // PS3 / Xbox: the zip the library points at was replaced by its ISO
            if name.to_lowercase().ends_with(".zip") && matches!(g.system.as_str(), "ps3" | "xbox360" | "xbox") {
                extra.push(p.with_extension("iso"));
                extra.push(p.with_extension("dkey"));
            }
            for q in std::iter::once(p).chain(extra) {
                if q.exists() && !out.contains(&q) {
                    out.push(q);
                }
            }
        }
        out
    }

    /// `X`: delete the selected game's files from the roms folder, after a second `X`.
    /// Games from a source stay in the library (Enter downloads them again); entries the
    /// roms-folder scan found are removed, since nothing else is left of them.
    fn delete_selected_from_disk(&mut self) {
        let Some(g) = self.selected_game().cloned() else { return };
        if matches!(self.local.get(&g.id), Some(LocalState::Downloading { .. })) {
            self.status = "it's downloading right now; wait for it to finish first".into();
            return;
        }
        let files = self.files_on_disk(&g);
        if files.is_empty() {
            self.confirm_delete = None;
            self.status = format!("nothing of {} is on disk", g.name);
            return;
        }
        let confirmed = self.confirm_delete.take().is_some_and(|(id, at)| id == g.id && at.elapsed() < Duration::from_secs(5));
        if !confirmed {
            let size: u64 = files.iter().map(|f| disk_size(f)).sum();
            self.confirm_delete = Some((g.id.clone(), Instant::now()));
            self.status = format!("press X again to delete {} file(s), {}, of {} from disk", files.len(), crate::ui::human(size), g.name);
            return;
        }
        let mut failed = Vec::new();
        for f in &files {
            let res = if f.is_dir() { std::fs::remove_dir_all(f) } else { std::fs::remove_file(f) };
            if let Err(e) = res {
                failed.push(format!("{}: {e}", f.display()));
            }
        }
        if !failed.is_empty() {
            self.status = format!("couldn't delete {}", failed.join("; "));
            self.refresh_presence();
            return;
        }
        self.needs_key.remove(&g.id);
        self.local.remove(&g.id);
        if g.kind == SourceKind::Local {
            self.remove_selected();
        } else if let Some(game) = self.lib.get_mut(&g.id) {
            game.local_files.clear();
            self.save_lib();
            self.refresh_presence();
        }
        self.status = format!("deleted {} from disk ({} file(s))", g.name, files.len());
    }

    // ---------- input ----------

    fn current_input(&mut self) -> &mut String {
        match self.tab {
            Tab::Search => &mut self.rom_query,
            Tab::Library if self.lib_input == LibInput::Paste => &mut self.lib_paste,
            Tab::Config if self.cfg_tab.edit.is_some() => &mut self.cfg_tab.edit.as_mut().expect("checked").text,
            Tab::Library | Tab::Textures | Tab::Sources | Tab::Config => &mut self.lib_filter, // no text box
        }
    }

    fn after_input_change(&mut self) {
        match self.tab {
            Tab::Search => self.on_query_change(),
            Tab::Library if self.lib_input == LibInput::Filter => self.fix_cursor(),
            Tab::Library | Tab::Textures | Tab::Sources | Tab::Config => {}
        }
    }

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if !(self.editing && self.pad_typing) {
                    self.pad = false;
                    self.pad_typing = false;
                }
                self.on_key(k);
            }
            Event::Paste(s) if self.editing => {
                self.current_input().push_str(s.trim_end_matches(['\n', '\r']));
                self.after_input_change();
            }
            _ => {}
        }
    }

    /// Controller input. D-pad moves, A does the item's default action (like Enter), B opens
    /// its menu of actions, Y the tab's menu, X searches, LB/RB switch tabs, LT/RT page. Buttons do the same thing whether or not
    /// a text box or menu is open.
    pub fn on_pad(&mut self, p: Pad) {
        self.pad = true;
        self.on_pad_inner(p);
        self.pad_typing = self.editing;
    }

    fn on_pad_inner(&mut self, p: Pad) {
        use KeyCode as K;
        if self.menu.is_some() {
            let code = match p {
                Pad::A => K::Enter,
                Pad::B | Pad::Y | Pad::Menu => K::Esc,
                Pad::Up => K::Up,
                Pad::Down => K::Down,
                Pad::Left => K::Left,
                Pad::Right => K::Right,
                _ => return,
            };
            return self.press(code);
        }
        if self.editing {
            match p {
                Pad::A | Pad::X => return self.press(K::Enter),
                Pad::B => return self.press(K::Esc),
                Pad::Down => return self.press(K::Down),
                Pad::Up | Pad::Left | Pad::Right => return,
                _ => self.editing = false,
            }
        }
        let code = match p {
            Pad::A => K::Enter,
            Pad::B => K::Char(' '),
            Pad::X | Pad::View if matches!(self.tab, Tab::Textures | Tab::Sources | Tab::Config) => return,
            Pad::X | Pad::View => {
                self.press(K::Char('/'));
                gamepad::open_steam_keyboard();
                return;
            }
            Pad::Y | Pad::Menu => K::Char('m'),
            Pad::LB => K::BackTab,
            Pad::RB => K::Tab,
            Pad::LT => K::PageUp,
            Pad::RT => K::PageDown,
            Pad::Up => K::Up,
            Pad::Down => K::Down,
            Pad::Left => K::Left,
            Pad::Right => K::Right,
            Pad::L3 | Pad::R3 => return,
        };
        self.press(code);
    }

    // ---------- menus ----------

    /// Space / B on a row: that item's actions.
    fn open_item_menu(&mut self) {
        let mut items = Vec::new();
        let title;
        match self.tab {
            Tab::Textures => return self.tex_item_menu(),
            Tab::Sources => return self.set_item_menu(),
            Tab::Config => return self.cfg_item_menu(),
            Tab::Search => {
                let Some(hits) = self.selected_hits().map(<[usize]>::to_vec) else {
                    let Some(sys) = self.search_cursor_system() else { return };
                    let collapsed = self.search_collapsed.contains(&sys);
                    self.menu = Some(Menu {
                        title: systems::label(&sys).to_string(),
                        items: vec![(if collapsed { "Expand" } else { "Collapse" }.into(), Act::Fold)],
                        sel: 0,
                    });
                    return;
                };
                let h = &self.rom_hits[hits[0]];
                title = h.title.clone().unwrap_or_else(|| h.name.rsplit('/').next().unwrap_or(&h.name).to_string());
                let in_lib = hits.iter().any(|&i| self.lib.contains(&self.rom_hits[i].url));
                items.push((if in_lib { "Already in library" } else { "Add to library (best source)" }.into(), Act::AddRom));
                if hits.len() > 1 {
                    items.push((format!("Choose a source… ({})", hits.len()), Act::Sources));
                }
                items.push((system_label(&h.system), Act::System));
            }
            Tab::Library => match self.lib_cursor.clone() {
                None => return,
                Some(RowKey::More(sys)) => return self.show_all_of(&sys),
                Some(RowKey::Header(sys)) => {
                    title = systems::label(&sys).to_string();
                    let collapsed = self.collapsed.contains(&sys);
                    items.push((if collapsed { "Expand" } else { "Collapse" }.into(), Act::Fold));
                    if crate::metacritic::top_list(&sys).is_some() {
                        items.push((format!("Add the top {TOP_N} games"), Act::TopGames));
                    }
                    items.push(("Show all of this system".into(), Act::ShowAll));
                }
                Some(RowKey::Game(_)) => {
                    let Some(g) = self.selected_game().cloned() else { return };
                    title = self.display_name(&g.system, &g.name);
                    let present = self.present.contains(&g.id);
                    if self.needs_key.contains(&g.id) {
                        items.push(("Get the disc key".into(), Act::Download));
                    } else if !present && g.kind == SourceKind::Vimm {
                        items.push(("Open Vimm's page in a browser".into(), Act::Download));
                    } else if !present {
                        items.push(("Download".into(), Act::Download));
                        if g.kind == SourceKind::Link {
                            items.push(("Download directly (skip TorBox)".into(), Act::DownloadDirect));
                        }
                    }
                    if !present {
                        items.push((system_label(&g.system), Act::System));
                    }
                    items.push(("Remove from library".into(), Act::Remove));
                    if !self.files_on_disk(&g).is_empty() {
                        items.push(("Delete from disk…".into(), Act::DeleteAsk));
                    }
                }
            },
        }
        self.menu = Some(Menu { title, items, sel: 0 });
    }

    /// `m` / Y: the current tab's menu.
    fn open_tab_menu(&mut self) {
        let mut items: Vec<(String, Act)> = Vec::new();
        let title = match self.tab {
            Tab::Search => {
                items.push(("Search…".into(), Act::Search));
                "Search"
            }
            Tab::Library => {
                items.push(("Filter…".into(), Act::Search));
                items.push(("Download all ready".into(), Act::DownloadAll));
                items.push(("Paste a link…".into(), Act::Paste));
                items.push((
                    match self.lib_sort {
                        LibSort::Name => "Sort by size",
                        LibSort::Size => "Sort by name",
                    }
                    .into(),
                    Act::Sort,
                ));
                items.push(("Collapse all".into(), Act::CollapseAll));
                items.push(("Expand all".into(), Act::ExpandAll));
                items.push(("Rescan roms folder and TorBox".into(), Act::Rescan));
                "Library"
            }
            Tab::Textures => {
                items.push((
                    if self.tex.loading { "Fetching the texture pack lists…" } else { "Refresh the texture pack lists" }.into(),
                    Act::TexRefresh,
                ));
                "Textures"
            }
            Tab::Sources => {
                items.push(("Open all groups".into(), Act::ExpandAll));
                items.push(("Close all groups".into(), Act::CollapseAll));
                "Sources"
            }
            Tab::Config => {
                if !self.cfg_tab.restart_for.is_empty() {
                    items.push(("Restart romgetter (applies the changes)".into(), Act::Cfg(CfgAct::Restart)));
                }
                "Config"
            }
        };
        items.push(("Quit romgetter".into(), Act::Quit));
        self.menu = Some(Menu { title: title.into(), items, sel: 0 });
    }

    fn on_menu_key(&mut self, k: KeyEvent) {
        let Some(m) = self.menu.as_mut() else { return };
        let len = m.items.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q' | 'm') => self.menu = None,
            KeyCode::Up | KeyCode::Char('k') => m.sel = (m.sel + len - 1) % len,
            KeyCode::Down | KeyCode::Char('j') => m.sel = (m.sel + 1) % len,
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') => {
                if m.items[m.sel].1 == Act::System {
                    let delta = if matches!(k.code, KeyCode::Left | KeyCode::Char('h')) { -1 } else { 1 };
                    self.change_system_in_menu(delta);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let act = m.items[m.sel].1;
                if act == Act::System {
                    return self.change_system_in_menu(1);
                }
                self.menu = None;
                self.run(act);
            }
            _ => {}
        }
    }

    fn change_system_in_menu(&mut self, delta: isize) {
        self.cycle_system(delta);
        let sys = match self.tab {
            Tab::Search => self.selected_hit().map(|h| h.system.clone()),
            Tab::Library => self.selected_game().map(|g| g.system.clone()),
            Tab::Textures | Tab::Sources | Tab::Config => None,
        };
        if let (Some(sys), Some(m)) = (sys, self.menu.as_mut()) {
            m.items[m.sel].0 = system_label(&sys);
        }
    }

    fn run(&mut self, act: Act) {
        match act {
            Act::AddRom => self.add_selected_rom(),
            Act::AddSource(i) => self.add_hit(i),
            Act::Sources => self.open_source_menu(),
            Act::Download => self.download_selected(false),
            Act::DownloadDirect => self.download_selected(true),
            Act::System | Act::Close => {} // System changes in place (on_menu_key); Close just closes
            Act::Remove => self.remove_selected(),
            Act::DeleteAsk => {
                self.confirm_delete = None;
                self.delete_selected_from_disk(); // first call only asks; sets confirm_delete
                if self.confirm_delete.is_some() {
                    let title = std::mem::take(&mut self.status).replace("press X again to delete", "Delete");
                    self.menu = Some(Menu {
                        title: format!("{title}?"),
                        items: vec![("Cancel".into(), Act::Close), ("Delete from disk".into(), Act::DeleteYes)],
                        sel: 0,
                    });
                }
            }
            Act::DeleteYes => {
                self.confirm_delete = self.selected_game().map(|g| (g.id.clone(), Instant::now()));
                self.delete_selected_from_disk();
            }
            Act::Fold if self.tab == Tab::Sources => {
                if let Some(g) = self.sets_cursor_group() {
                    let open = self.sets_open.contains(g);
                    self.set_group_open(g, !open);
                }
            }
            Act::Fold if self.tab == Tab::Search => {
                if let Some(sys) = self.search_cursor_system() {
                    let c = self.search_collapsed.contains(&sys);
                    self.set_search_collapsed(&sys, !c);
                }
            }
            Act::Fold => self.toggle_current_group(),
            Act::TopGames => self.add_top_games(),
            Act::ShowAll => {
                if let Some(sys) = self.cursor_system() {
                    self.show_all_of(&sys);
                }
            }
            Act::Search => {
                if self.tab == Tab::Library {
                    self.lib_input = LibInput::Filter;
                }
                self.editing = true;
                gamepad::open_steam_keyboard();
            }
            Act::DownloadAll => self.download_all_ready(),
            Act::Paste => {
                self.lib_input = LibInput::Paste;
                self.editing = true;
                gamepad::open_steam_keyboard();
            }
            Act::Sort => self.press(KeyCode::Char('o')),
            Act::CollapseAll if self.tab == Tab::Sources => self.sets_open.clear(),
            Act::ExpandAll if self.tab == Tab::Sources => self.sets_open = SET_GROUPS.iter().map(ToString::to_string).collect(),
            Act::CollapseAll => self.set_all_collapsed(true),
            Act::ExpandAll => self.set_all_collapsed(false),
            Act::SetSearch => self.search_set_system(),
            Act::Reindex => self.reindex_selected_set(),
            Act::Rescan => self.press(KeyCode::Char('r')),
            Act::ToggleSource => self.toggle_source_row(),
            Act::CacheTo(d) => self.cache_selected_set(d),
            Act::Cfg(a) => self.cfg_run(a),
            Act::TexInstall(_) | Act::TexOpenUrl(_) | Act::TexToggle | Act::TexRemove | Act::TexPage | Act::TexRefresh => self.tex_run(act),
            Act::Quit => self.quit = true,
        }
    }

    fn press(&mut self, code: KeyCode) {
        self.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.menu.is_some() {
            return self.on_menu_key(k);
        }
        if self.editing {
            match k.code {
                KeyCode::Esc | KeyCode::Down => {
                    self.editing = false;
                    self.cfg_cancel();
                    if self.tab == Tab::Library && self.lib_input == LibInput::Paste && k.code == KeyCode::Esc {
                        self.lib_paste.clear();
                        self.lib_input = LibInput::Filter;
                    }
                }
                KeyCode::Tab => {
                    self.editing = false;
                    self.cfg_cancel();
                    self.next_tab(1);
                }
                KeyCode::Enter => {
                    self.editing = false;
                    match self.tab {
                        Tab::Search => {
                            // search archive.org now instead of waiting for the debounce
                            if self.live_due.take().is_some() {
                                self.start_live_search();
                            }
                        }
                        Tab::Library if self.lib_input == LibInput::Paste => self.add_pasted_link(),
                        Tab::Config => self.cfg_submit(),
                        Tab::Library | Tab::Textures | Tab::Sources => {}
                    }
                }
                KeyCode::Backspace => {
                    self.current_input().pop();
                    self.after_input_change();
                }
                KeyCode::Char(c) => {
                    self.current_input().push(c);
                    self.after_input_change();
                }
                _ => {}
            }
            return;
        }

        if k.code != KeyCode::Char('X') {
            self.confirm_delete = None; // any other key cancels a pending delete
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('1') => self.set_tab(Tab::Search),
            KeyCode::Char('2') => self.set_tab(Tab::Library),
            KeyCode::Char('3') => self.set_tab(Tab::Textures),
            KeyCode::Char('4') => self.set_tab(Tab::Sources),
            KeyCode::Char('5') => self.set_tab(Tab::Config),
            KeyCode::Tab => self.next_tab(1),
            KeyCode::BackTab => self.next_tab(-1),
            KeyCode::Char('/' | 'i') if matches!(self.tab, Tab::Textures | Tab::Sources | Tab::Config) => {}
            KeyCode::Char('/' | 'i') => {
                if self.tab == Tab::Library {
                    self.lib_input = LibInput::Filter;
                }
                self.editing = true;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(15),
            KeyCode::PageUp => self.move_sel(-15),
            // s: the main Search box from any tab (/ searches within the current tab)
            KeyCode::Char('s') => {
                self.set_tab(Tab::Search);
                self.editing = true;
            }
            KeyCode::Char('S') => self.cycle_system(1),
            KeyCode::Char('m') => self.open_tab_menu(),
            code => match (self.tab, code) {
                (Tab::Search, KeyCode::Enter) => self.activate_search_row(),
                (Tab::Search, KeyCode::Char('a')) => self.add_selected_rom(),
                (Tab::Search, KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l')) => {
                    if let Some(sys) = self.search_cursor_system() {
                        self.set_search_collapsed(&sys, matches!(code, KeyCode::Left | KeyCode::Char('h')));
                    }
                }
                (Tab::Textures, KeyCode::Enter) => self.tex_activate(),
                (Tab::Sources, KeyCode::Enter) => self.activate_set_row(),
                (Tab::Config, KeyCode::Enter) => self.cfg_activate(),
                (Tab::Sources, KeyCode::Char(' ')) => self.toggle_source_row(),
                (Tab::Sources, KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l')) => {
                    if let Some(g) = self.sets_cursor_group() {
                        let open = matches!(code, KeyCode::Right | KeyCode::Char('l'));
                        self.set_group_open(g, open);
                    }
                }
                (Tab::Textures, KeyCode::Char('r')) => self.tex_fetch(),
                (Tab::Library, KeyCode::Enter) => self.activate_row(),
                (_, KeyCode::Char(' ')) => self.open_item_menu(),
                (Tab::Library, KeyCode::Left | KeyCode::Char('h')) => {
                    if let Some(sys) = self.cursor_system() {
                        self.set_collapsed(&sys, true);
                    }
                }
                (Tab::Library, KeyCode::Right | KeyCode::Char('l')) => {
                    if let Some(sys) = self.cursor_system() {
                        self.set_collapsed(&sys, false);
                    }
                }
                (Tab::Library, KeyCode::Char('c')) => self.set_all_collapsed(true),
                (Tab::Library, KeyCode::Char('e')) => self.set_all_collapsed(false),
                (Tab::Library, KeyCode::Char('d')) => self.download_selected(true),
                (Tab::Library, KeyCode::Char('D')) => self.download_all_ready(),
                (Tab::Library, KeyCode::Char('t')) => self.add_top_games(),
                (Tab::Library, KeyCode::Char('p')) => {
                    self.lib_input = LibInput::Paste;
                    self.editing = true;
                }
                (Tab::Library, KeyCode::Char('x')) => self.remove_selected(),
                (Tab::Library, KeyCode::Char('X')) => self.delete_selected_from_disk(),
                (Tab::Library, KeyCode::Char('o')) => {
                    self.lib_sort = match self.lib_sort {
                        LibSort::Name => LibSort::Size,
                        LibSort::Size => LibSort::Name,
                    };
                    self.status = match self.lib_sort {
                        LibSort::Name => "library sorted by name (o: by size)",
                        LibSort::Size => "library sorted by size, biggest first (o: by name)",
                    }
                    .into();
                    self.fix_cursor();
                }
                (Tab::Library, KeyCode::Char('r')) => {
                    self.last_poll = None;
                    self.status = "rescanning roms folder and refreshing TorBox…".into();
                    self.start_scan();
                }
                _ => {}
            },
        }
    }

    fn cycle_system(&mut self, delta: isize) {
        match self.tab {
            Tab::Search => {
                // every source of the game moves; the row regroups on the next refresh
                let Some(hits) = self.selected_hits().map(<[usize]>::to_vec) else { return };
                let sys = systems::cycle(&self.rom_hits[hits[0]].system, delta).to_string();
                for i in hits {
                    self.rom_hits[i].system.clone_from(&sys);
                }
            }
            Tab::Textures | Tab::Sources | Tab::Config => {}
            Tab::Library => {
                let roms = self.ctx.roms_dir.clone();
                let Some(id) = self.selected_game().map(|g| g.id.clone()) else { return };
                let Some(g) = self.lib.get_mut(&id) else { return };
                if g.is_present(&roms) {
                    self.status = "already downloaded; remove it (x) and re-add to move it".into();
                    return;
                }
                g.system = systems::cycle(&g.system, delta).to_string();
                let sys = g.system.clone();
                self.collapsed.remove(&sys); // follow the game to its new group
                self.save_lib();
                self.refresh_presence();
            }
        }
    }

    /// Tab / RB: the next tab; Shift-Tab / LB (`delta` -1): the previous one.
    fn next_tab(&mut self, delta: isize) {
        let n = Tab::ALL.len() as isize;
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0) as isize;
        self.set_tab(Tab::ALL[(i + delta).rem_euclid(n) as usize]);
    }

    fn set_tab(&mut self, t: Tab) {
        self.tab = t;
        if t == Tab::Textures {
            self.tex_open();
        }
        if t == Tab::Config {
            self.cfg_open();
        }
    }

    fn move_sel(&mut self, delta: isize) {
        if self.tab == Tab::Textures {
            return self.tex_move(delta);
        }
        if self.tab == Tab::Config {
            return self.cfg_move(delta);
        }
        if self.tab == Tab::Sources {
            let keys: Vec<String> = self.set_rows().iter().map(Self::set_row_key).collect();
            if keys.is_empty() {
                return;
            }
            let cur = self.sets_cursor.as_ref().and_then(|c| keys.iter().position(|k| k == c)).unwrap_or(0) as isize;
            self.sets_cursor = Some(keys[(cur + delta).clamp(0, keys.len() as isize - 1) as usize].clone());
            return;
        }
        if self.tab == Tab::Library {
            let keys = self.row_keys();
            if keys.is_empty() {
                return;
            }
            let cur = self.lib_cursor.as_ref().and_then(|c| keys.iter().position(|k| k == c)).unwrap_or(0) as isize;
            if cur == 0 && delta < 0 {
                self.lib_input = LibInput::Filter;
                self.editing = true;
                return;
            }
            self.lib_cursor = Some(keys[(cur + delta).clamp(0, keys.len() as isize - 1) as usize].clone());
            return;
        }
        let len = self.search_rows.len();
        if len == 0 {
            return;
        }
        let cur = self.rom_state.selected().unwrap_or(0) as isize;
        if cur == 0 && delta < 0 {
            self.editing = true; // arrow up off the top goes back to the input box
            return;
        }
        self.rom_state.select(Some((cur + delta).clamp(0, len as isize - 1) as usize));
    }

    pub fn active_downloads(&self) -> usize {
        self.local.values().filter(|s| matches!(s, LocalState::Downloading { .. })).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_groups_sources_by_game() {
        let mut app = test_app(Vec::new());
        app.tab = Tab::Search;
        app.editing = false;
        app.tb_zips.insert(
            "aa".into(),
            tbzips::Candidate { id: 1, hash: "aa".into(), system: "psx".into(), title: "1G1R PS1".into(), zipped: true, inner: None },
        );
        let hit = |name: &str, system: &str, source: &str, live: bool, torrent: Option<&str>| RomHit {
            url: format!("{source}/{name}"),
            name: name.into(),
            size: 1,
            system: system.into(),
            source: source.into(),
            live,
            torrent: torrent.map(|h| (h.to_string(), None)),
            title: None,
        };
        app.local_hits = vec![
            hit("Crash Bandicoot (USA).zip", "psx", "redump-set", false, None),
            hit("Crash Bandicoot (USA).7z", "psx", "1G1R PS1", false, Some("aa")),
            hit("Crash Bandicoot (Japan).7z", "psx", "1G1R PS1", false, Some("aa")),
            hit("Super Mario World (USA).zip", "snes", "nointro", false, None),
        ];
        app.live_hits = vec![hit("crash_bandicoot_(usa).ZIP", "psx", "archive.org/x", true, None)];
        app.merge_hits();
        let shape: Vec<String> = app
            .search_rows
            .iter()
            .map(|r| match r {
                SearchRow::Header { system, count, .. } => format!("H {system} {count}"),
                SearchRow::Game(h) => format!("G {} {}", app.rom_hits[h[0]].source, h.len()),
            })
            .collect();
        assert!(shape.contains(&"H psx 1".to_string()) && shape.contains(&"H snes 1".to_string()), "{shape:?}");
        assert!(shape.contains(&"G 1G1R PS1 4".to_string()), "cached TorBox copy first: {shape:?}");
        assert!(matches!(app.search_rows[app.rom_state.selected().unwrap()], SearchRow::Game(_)), "starts on a game");

        // Enter on the PS1 game lists its sources, ⚡ first, live last
        let psx = app.search_rows.iter().position(|r| matches!(r, SearchRow::Game(h) if h.len() == 4)).unwrap();
        app.rom_state.select(Some(psx));
        app.press(KeyCode::Enter);
        let m = app.menu.as_ref().expect("source menu");
        let labels: Vec<&str> = m.items.iter().map(|(l, _)| l.as_str()).collect();
        // cached first (USA before Japan), then the uncached USA copies, live last
        assert!(labels[0].starts_with('⚡') && labels[0].contains("(USA)"), "{labels:?}");
        assert!(labels[1].starts_with('⚡') && labels[1].contains("(Japan)"), "{labels:?}");
        assert!(labels[2].contains("redump-set") && labels[3].contains("archive.org/x"), "{labels:?}");
        app.press(KeyCode::Down);
        app.press(KeyCode::Down);
        app.press(KeyCode::Enter);
        assert!(app.lib.contains("redump-set/Crash Bandicoot (USA).zip"));

        // Enter on a header folds its system
        let h = app.search_rows.iter().position(|r| matches!(r, SearchRow::Header { system, .. } if system == "psx")).unwrap();
        app.rom_state.select(Some(h));
        app.press(KeyCode::Enter);
        assert!(matches!(&app.search_rows[h], SearchRow::Header { collapsed: true, .. }));
        assert!(!app.search_rows.iter().any(|r| matches!(r, SearchRow::Game(x) if x.len() == 4)));
    }

    #[tokio::test]
    async fn sources_tab_groups_sets() {
        let mut app = test_app(Vec::new());
        let idx = |n: usize, restricted: bool| ItemIndex {
            restricted,
            files: (0..n)
                .map(|i| archive::RomFile {
                    name: format!("g{i}.zip"),
                    size: 10,
                    url: None,
                    torrent_index: None,
                    title: None,
                    system: None,
                })
                .collect(),
            system: None,
            title: String::new(),
            fetched: 0,
            btih: None,
        };
        for (item, sys) in [
            (minerva::set_key("Redump", "Sony - PlayStation"), "psx"),
            (minerva::set_key("No-Intro", "Nintendo - Game Boy"), "gb"),
            ("nointro.md".to_string(), "megadrive"),
            ("microsoft_xbox360_a_part1".to_string(), "xbox360"),
        ] {
            app.sets.push(SetRef { item, system: sys.into() });
        }
        app.put_index(minerva::set_key("Redump", "Sony - PlayStation"), idx(3, false));
        app.put_index("nointro.md".into(), idx(2, false));
        app.put_index("microsoft_xbox360_a_part1".into(), idx(5, true));
        app.index_errors.insert(minerva::set_key("No-Intro", "Nintendo - Game Boy"), "HTTP 500".into());
        app.tab = Tab::Sources;
        app.editing = false;
        let shape = |app: &App| -> Vec<String> {
            app.set_rows()
                .iter()
                .map(|r| match r {
                    SetRow::Group { name, sets, off, files, collapsed } => {
                        format!(
                            "G {name} {sets} {files}{}{}",
                            if *off > 0 { format!(" {off}off") } else { String::new() },
                            if *collapsed { " closed" } else { "" }
                        )
                    }
                    SetRow::Set(item) => format!("S {item}"),
                    SetRow::Live(key) => format!("L {key}"),
                })
                .collect()
        };
        assert_eq!(
            shape(&app),
            vec![
                "G Minerva · No-Intro 1 0 closed",
                "G Minerva · Redump 1 3 closed",
                "G archive.org · built-in sets 1 2 closed",
                "G archive.org · restricted (log in on the Config tab) 1 5 closed",
                "G Live search (as you type) 2 0 1off closed",
            ]
        );
        // Enter on the first group opens it; its failed set shows why
        app.press(KeyCode::Enter);
        assert_eq!(app.sets_cursor.as_deref(), Some("\u{1}Minerva · No-Intro"));
        assert_eq!(shape(&app)[1], format!("S {}", minerva::set_key("No-Intro", "Nintendo - Game Boy")));
        app.press(KeyCode::Down);
        let info = app.set_info(&minerva::set_key("No-Intro", "Nintendo - Game Boy"), "gb");
        assert!(matches!(info.state, SetState::Failed(ref e) if e == "HTTP 500"));
        assert_eq!(info.title, "Nintendo - Game Boy");
        assert!(matches!(app.set_info("microsoft_xbox360_a_part1", "xbox360").state, SetState::Locked));
        // Enter on a set: its actions; "Search" jumps to the Search tab filtered to its system
        app.press(KeyCode::Enter);
        let acts: Vec<Act> = app.menu.as_ref().unwrap().items.iter().map(|i| i.1).collect();
        assert_eq!(acts[..2], [Act::ToggleSource, Act::SetSearch]);
        app.press(KeyCode::Down);
        app.press(KeyCode::Enter);
        assert!(app.tab == Tab::Search && app.editing && app.rom_query == "gb ");
    }

    /// Space switches a set, a group or a live site off and on; off sets drop out of
    /// search, and the choice is saved (Vimm starts off; the old `use_vimm` carries over).
    #[tokio::test]
    async fn sources_switch_on_and_off() {
        let mut app = test_app(Vec::new());
        let path = app.ctx.roms_dir.parent().unwrap().join("sources-toggle.toml");
        app.config_file = Some(path.clone());
        let file = |n: &str| archive::RomFile { name: n.into(), size: 1, url: None, torrent_index: None, title: None, system: None };
        for (item, game) in [("set.a", "Crash Bandicoot (USA).zip"), ("set.b", "Crash Bandicoot (Europe).zip")] {
            app.sets.push(SetRef { item: item.into(), system: "psx".into() });
            app.put_index(
                item.into(),
                ItemIndex { restricted: false, files: vec![file(game)], system: None, title: item.into(), fetched: 0, btih: None },
            );
        }
        app.refresh_local_for("crash");
        assert_eq!(app.local_hits.len(), 2);
        app.tab = Tab::Sources;
        app.editing = false;
        app.sets_open.insert(SET_GROUPS[3].into());
        app.sets_cursor = Some("set.a".into());
        app.press(KeyCode::Char(' '));
        assert!(!app.cfg.source_on("set.a") && !app.set_info("set.a", "psx").on);
        assert_eq!(app.local_hits.iter().map(|h| h.source.as_str()).collect::<Vec<_>>(), vec!["set.b"]);
        // the group: some on → all off; then all back on
        app.sets_cursor = Some(format!("\u{1}{}", SET_GROUPS[3]));
        app.press(KeyCode::Char(' '));
        assert!(app.local_hits.is_empty());
        app.press(KeyCode::Char(' '));
        assert_eq!(app.local_hits.len(), 2);
        assert!(app.cfg.sources.is_empty(), "back at the defaults: {:?}", app.cfg.sources);
        // Vimm: off by default, Space turns it on and that's saved
        assert!(!app.cfg.source_on(LIVE_VIMM) && app.cfg.source_on(LIVE_ARCHIVE));
        app.sets_cursor = Some(format!("\u{2}{LIVE_VIMM}"));
        app.press(KeyCode::Char(' '));
        assert!(Config::load(&path).unwrap().source_on(LIVE_VIMM));
        // an old config's `use_vimm = true` becomes `[sources] vimm = true`
        std::fs::write(&path, "roms_dir = \"/tmp/roms\"\nuse_vimm = true\n").unwrap();
        let old = Config::load(&path).unwrap();
        assert!(old.source_on(LIVE_VIMM) && !toml::to_string(&old).unwrap().contains("use_vimm"));
        let _ = std::fs::remove_file(&path);
    }

    /// A torrent-backed set under 1 TB that a service doesn't have whole offers "Cache to
    /// <service>"; sets TorBox has whole, TorBox torrents, huge sets and sets being cached
    /// don't.
    #[tokio::test]
    async fn sources_offer_caching_to_debrid() {
        let mut app = test_app(Vec::new());
        app.ctx.torbox = Some(crate::torbox::TorBox::new(reqwest::Client::new(), "k".into()));
        app.ctx.rd = Some(crate::realdebrid::RealDebrid::new(reqwest::Client::new(), "k".into()));
        let set = |h: &str, size: u64| ItemIndex {
            restricted: false,
            files: vec![archive::RomFile { name: "a.zip".into(), size, url: None, torrent_index: Some(0), title: None, system: None }],
            system: Some("psx".into()),
            title: "Minerva · Redump · x".into(),
            fetched: 0,
            btih: Some(h.into()),
        };
        let small = minerva::set_key("Redump", "Sony - PlayStation");
        let huge = minerva::set_key("Redump", "Sony - PlayStation 3");
        let held = minerva::set_key("No-Intro", "Nintendo - Game Boy");
        let tbset = tbzips::set_key("dd");
        for (k, i) in
            [(&small, set("AA", 5 << 30)), (&huge, set("bb", 2 * CACHE_MAX)), (&held, set("cc", 1 << 20)), (&tbset, set("dd", 1 << 30))]
        {
            app.sets.push(SetRef { item: k.clone(), system: "psx".into() });
            app.put_index(k.clone(), i);
        }
        app.tb_held.insert("cc".into(), None);
        assert_eq!(app.cache_targets(&small), vec![Debrid::TorBox, Debrid::RealDebrid]);
        assert_eq!(app.cache_targets(&huge), vec![]);
        assert_eq!(app.cache_targets(&held), vec![Debrid::RealDebrid]);
        assert_eq!(app.cache_targets(&tbset), vec![]);
        // the set's menu lists them; once one is sent, it's no longer offered
        app.tab = Tab::Sources;
        app.editing = false;
        app.sets_cursor = Some(small.clone());
        app.press(KeyCode::Enter);
        let acts: Vec<Act> = app.menu.as_ref().unwrap().items.iter().map(|i| i.1).collect();
        assert!(acts.contains(&Act::CacheTo(Debrid::TorBox)) && acts.contains(&Act::CacheTo(Debrid::RealDebrid)), "{acts:?}");
        app.cache_jobs.insert((Debrid::TorBox, "aa".into()), CacheJob { state: "downloading".into(), progress: 0.4, done: false });
        assert_eq!(app.cache_targets(&small), vec![Debrid::RealDebrid]);
        assert_eq!(app.set_info(&small, "psx").caching, vec![("TorBox", "downloading".to_string(), 0.4, false)]);
    }

    #[tokio::test]
    async fn config_tab_changes_settings() {
        let mut app = test_app(Vec::new());
        app.editing = false;
        app.press(KeyCode::Char('5'));
        assert!(app.tab == Tab::Config);
        let typed = |app: &mut App, s: &str| {
            for c in s.chars() {
                app.press(KeyCode::Char(c));
            }
            app.press(KeyCode::Enter);
        };
        let go = |app: &mut App, f: Field| {
            app.cfg_tab.cursor = 0;
            while app.cfg_field() != Some(f) {
                let before = app.cfg_tab.cursor;
                app.press(KeyCode::Down);
                assert_ne!(before, app.cfg_tab.cursor, "{f:?} not found");
            }
        };

        // a key: typed hidden, saved, and needs a restart
        go(&mut app, Field::RdKey);
        app.press(KeyCode::Enter);
        assert!(app.editing && app.cfg_tab.edit.as_ref().is_some_and(settings::CfgEdit::secret));
        typed(&mut app, "abcdefghijkl");
        assert_eq!(app.cfg.real_debrid_api_key.as_deref(), Some("abcdefghijkl"));
        assert!(!app.editing && app.cfg_tab.restart_for == ["the Real-Debrid key"]);
        assert_eq!(app.cfg_field(), Some(Field::RdKey), "the restart row above keeps the cursor on the key");

        // Esc leaves it as it was
        go(&mut app, Field::Poll);
        app.press(KeyCode::Enter);
        assert_eq!(app.cfg_tab.edit.as_ref().map(|e| e.text.as_str()), Some("5"), "starts from the current value");
        app.press(KeyCode::Char('0'));
        app.press(KeyCode::Esc);
        assert_eq!(app.cfg.poll_secs, 5);
        // applies at once
        app.press(KeyCode::Enter);
        app.press(KeyCode::Backspace);
        typed(&mut app, "9");
        assert!(app.cfg.poll_secs == 9 && app.ctx.poll == Duration::from_secs(9));
        app.press(KeyCode::Enter);
        app.press(KeyCode::Backspace);
        typed(&mut app, "1");
        assert_eq!(app.cfg.poll_secs, 9, "too short: {}", app.status);

        // extra sets: added from a URL and indexed, removed from the row's menu
        go(&mut app, Field::AddSet);
        app.press(KeyCode::Enter);
        typed(&mut app, "https://archive.org/details/some-set_1");
        assert_eq!(app.cfg.extra_sets.iter().map(|c| c.item.as_str()).collect::<Vec<_>>(), ["some-set_1"]);
        assert!(app.sets.iter().any(|s| s.item == "some-set_1"));
        assert_eq!(app.cfg_field(), Some(Field::ExtraSet(0)));
        app.press(KeyCode::Enter);
        let m = app.menu.as_ref().unwrap();
        assert_eq!(m.items[0].0, "Remove this set");
        app.press(KeyCode::Enter);
        assert!(app.cfg.extra_sets.is_empty() && !app.sets.iter().any(|s| s.item == "some-set_1"));

        // archive.org: email, then a hidden password
        go(&mut app, Field::Archive);
        app.press(KeyCode::Enter);
        typed(&mut app, "me@example.com");
        assert!(app.cfg_tab.edit.as_ref().is_some_and(|e| e.secret() && e.email.as_deref() == Some("me@example.com")));
        app.press(KeyCode::Esc);
        assert!(app.cfg_tab.edit.is_none() && !app.cfg_tab.logging_in);

        // restart: from the top row
        go(&mut app, Field::Restart);
        app.press(KeyCode::Enter);
        assert!(app.restart && app.quit);
    }

    #[tokio::test]
    async fn old_group_switches_become_sources_switches() {
        let dir = std::env::temp_dir().join(format!("romgetter-groups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, format!("roms_dir = {:?}\nuse_ifarchive = false\nuse_minerva = false\n", dir.join("roms"))).unwrap();
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.use_ifarchive && cfg.use_minerva, "groups always load now");
        assert!(!toml::to_string(&cfg).unwrap().contains("use_"), "and the old switches aren't written back");
        let cfg = Config { use_default_sets: false, ..cfg };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (local_tx, _lrx) = tokio::sync::mpsc::unbounded_channel();
        let ctx =
            jobs::Ctx { http: reqwest::Client::new(), torbox: None, rd: None, roms_dir: dir.join("roms"), poll: Duration::from_secs(5) };
        let mut app = App::new(cfg, dir.join("cache"), Library::load(dir.join("library.json")).unwrap(), tx, local_tx, ctx);
        assert!(app.sets.iter().any(|s| s.item == ifarchive::KEY), "the IF Archive is listed");
        assert!(!app.cfg.source_on(ifarchive::KEY), "but switched off");
        // a Minerva folder list arriving later: its sets come in off too
        let folder = minerva::Folder { name: "Nintendo - Super Nintendo Entertainment System".into(), size: 1 };
        app.on_msg(Msg::MinervaFolders("No-Intro".into(), Ok(vec![folder])));
        let key = minerva::set_key("No-Intro", "Nintendo - Super Nintendo Entertainment System");
        assert!(app.sets.iter().any(|s| s.item == key) && !app.cfg.source_on(&key));
        // and Space on the group header turns the group back on
        app.tab = Tab::Sources;
        app.editing = false;
        app.sets_cursor = Some(format!("\u{1}{}", SET_GROUPS[6]));
        app.press(KeyCode::Char(' '));
        assert!(app.cfg.source_on(ifarchive::KEY));
    }

    fn test_app(games: Vec<Game>) -> App {
        // one folder per test app: tests run in parallel and write to their roms folders
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("romgetter-app-{}-{n}", std::process::id()));
        let cfg =
            Config { use_minerva: false, use_default_sets: false, use_ifarchive: false, roms_dir: dir.join("roms"), ..Config::default() };
        let mut lib = Library::load(dir.join("library.json")).unwrap();
        lib.games = games;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (local_tx, _lrx) = tokio::sync::mpsc::unbounded_channel();
        let ctx =
            jobs::Ctx { http: reqwest::Client::new(), torbox: None, rd: None, roms_dir: dir.join("roms"), poll: Duration::from_secs(5) };
        App::new(cfg, dir.join("cache"), lib, tx, local_tx, ctx)
    }

    fn games(system: &str, n: usize) -> Vec<Game> {
        (0..n)
            .map(|i| Game::new(format!("{system} game {i:02}"), system.into(), SourceKind::Link, format!("https://x/{system}/{i}"), 1))
            .collect()
    }

    fn shape(app: &App) -> Vec<String> {
        app.lib_rows()
            .iter()
            .map(|r| match r {
                Row::Header { system, count, collapsed } => format!("H {system} {count}{}", if *collapsed { " collapsed" } else { "" }),
                Row::Game(_) => "g".into(),
                Row::More { system, hidden } => format!("M {system} {hidden}"),
            })
            .collect()
    }

    #[test]
    fn recognises_full_partial_and_packed_caches() {
        let set = |v: &[&str]| v.iter().map(ToString::to_string).collect::<HashSet<String>>();
        let names = set(&["a (usa).zip", "b (usa).zip", "c (usa).zip"]);
        assert!(fully_cached(&names, &set(&["a (usa).zip", "b (usa).zip", "c (usa).zip"])));
        assert!(fully_cached(&names, &set(&["minerva_myrient.zip"])), "TorBox packed the torrent into one zip");
        assert!(!fully_cached(&names, &set(&["b (usa).zip"])), "one real game = partial");
        assert!(!fully_cached(&names, &set(&[])));
    }

    #[tokio::test]
    async fn hides_minerva_files_torbox_lacks() {
        let mut app = test_app(Vec::new());
        let h = "626a5e05f498e435145c97352aa2fecf7e95a668".to_string();
        let file =
            |n: &str| archive::RomFile { name: n.into(), size: 14 << 30, url: None, torrent_index: Some(1), title: None, system: None };
        app.sets.push(SetRef { item: minerva::set_key("Redump", "Sony - PlayStation 3"), system: "ps3".into() });
        app.put_index(
            minerva::set_key("Redump", "Sony - PlayStation 3"),
            ItemIndex {
                restricted: false,
                files: vec![file("Armored Core 4 (USA).zip"), file("Batman - Arkham City (USA).zip")],
                system: Some("ps3".into()),
                title: "Minerva · Redump · Sony - PlayStation 3".into(),
                fetched: 0,
                btih: Some(h.clone()),
            },
        );
        app.rom_query = "ps3".into();
        app.refresh_local();
        assert_eq!(app.local_hits.len(), 2, "unknown cache status: show everything");
        app.minerva_cache.insert(h.clone(), TorrentCache { files: Some(["batman - arkham city (usa).zip".to_string()].into()) });
        app.refresh_local();
        let names: Vec<&str> = app.local_hits.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["Batman - Arkham City (USA).zip"]);
        app.minerva_cache.insert(h.clone(), TorrentCache { files: None });
        app.refresh_local();
        assert_eq!(app.local_hits.len(), 2, "fully cached: show everything");
        // a small folder TorBox hasn't cached at all can still be downloaded whole
        app.minerva_cache.insert(h.clone(), TorrentCache { files: Some(HashSet::new()) });
        for f in &mut Arc::make_mut(app.index.get_mut(&minerva::set_key("Redump", "Sony - PlayStation 3")).unwrap()).files {
            f.size = 1 << 20;
        }
        assert!(app.minerva_available(&h, "Armored Core 4 (USA).zip"));
    }

    #[tokio::test]
    async fn routes_uncached_minerva_games_to_real_debrid() {
        let mut app = test_app(Vec::new());
        let h = "626a5e05f498e435145c97352aa2fecf7e95a668".to_string();
        app.put_index(
            minerva::set_key("Redump", "Sony - PlayStation 3"),
            ItemIndex {
                restricted: false,
                files: vec![archive::RomFile {
                    name: "Armored Core 4 (USA).zip".into(),
                    size: 40 << 40,
                    url: None,
                    torrent_index: Some(171),
                    title: None,
                    system: None,
                }],
                system: Some("ps3".into()),
                title: "Minerva · Redump · Sony - PlayStation 3".into(),
                fetched: 0,
                btih: Some(h.clone()),
            },
        );
        app.minerva_cache.insert(h.clone(), TorrentCache { files: Some(HashSet::new()) });
        let g = Game::torrent_file("m".into(), "Armored Core 4 (USA).zip".into(), "ps3".into(), minerva::magnet(&h), Some(171), 1);
        let snes = Game::new("Zelda.zip".into(), "snes".into(), SourceKind::Link, "https://x/zelda.zip".into(), 1);
        app.ctx.torbox = Some(crate::torbox::TorBox::new(reqwest::Client::new(), "k".into()));
        assert!(!app.uses_rd(&g), "no Real-Debrid key: TorBox (and its 'not cached' error)");
        app.ctx.rd = Some(crate::realdebrid::RealDebrid::new(reqwest::Client::new(), "k".into()));
        assert!(app.uses_rd(&g), "TorBox can't deliver it, Real-Debrid can");
        assert!(!app.uses_rd(&snes), "non-Minerva games stay on TorBox");
        app.minerva_cache.insert(h, TorrentCache { files: None });
        assert!(!app.uses_rd(&g), "TorBox has it cached: TorBox");
    }

    #[tokio::test]
    async fn top_list_matching() {
        let mut app = test_app(Vec::new());
        let file = |n: &str| archive::RomFile { name: n.into(), size: 1, url: None, torrent_index: None, title: None, system: None };
        app.sets.push(SetRef { item: "x360".into(), system: "xbox360".into() });
        app.put_index(
            "x360".into(),
            ItemIndex {
                restricted: false,
                files: vec![
                    file("Halo 3 (USA).iso"),
                    file("Halo 3 - ODST (USA).iso"),
                    file("Wave Race 64 - Kawasaki Jet Ski (USA).iso"),
                    file("Wave Race 64 - Kawasaki Jet Ski (Japan).iso"),
                    file("Ninja Gaiden II (USA) (Beta).iso"),
                    file("Fable II (Europe).iso"),
                    file("Fable II (USA).iso"),
                    file("Forza 2 - Track Pack (USA).iso"),
                    file("Forza 2 - Car Pack (USA).iso"),
                ],
                system: Some("xbox360".into()),
                title: String::new(),
                fetched: 0,
                btih: None,
            },
        );
        let titles: Vec<String> = ["Halo 3", "Wave Race 64", "Ninja Gaiden II", "Fable II", "Forza 2"].map(String::from).into();
        let got: Vec<Option<String>> = app.best_hits_for("xbox360", &titles).into_iter().map(|(_, h)| h.map(|h| h.name)).collect();
        assert_eq!(got[0].as_deref(), Some("Halo 3 (USA).iso"), "exact beats a subtitle");
        assert_eq!(got[1].as_deref(), Some("Wave Race 64 - Kawasaki Jet Ski (USA).iso"), "unique subtitle, best region");
        assert_eq!(got[2], None, "betas don't count");
        assert_eq!(got[3].as_deref(), Some("Fable II (USA).iso"));
        assert_eq!(got[4], None, "two different subtitled games: ambiguous");
    }

    #[tokio::test]
    async fn if_archive_games_skip_torbox() {
        let url = "https://ifarchive.org/if-archive/games/zcode/photopia.z5";
        let mut g = Game::new("photopia.z5".into(), "zmachine".into(), SourceKind::Link, url.into(), 239616);
        g.title = Some("Photopia, by Adam Cadre".into());
        let mut app = test_app(vec![g]);
        assert!(matches!(app.remote.get(url), Some(Remote::Direct)));
        app.ctx.torbox = Some(crate::torbox::TorBox::new(reqwest::Client::new(), "k".into()));
        app.remote.insert(url.into(), Remote::NotSent);
        app.auto_send();
        assert!(matches!(app.remote.get(url), Some(Remote::NotSent)), "auto_send must leave it alone");
        app.send_to_torbox(url);
        assert!(matches!(app.remote.get(url), Some(Remote::Direct)));
        let g = app.lib.get(url).unwrap();
        assert_eq!(app.game_name(g), "Photopia, by Adam Cadre  [photopia.z5]");
        // the Library filter finds it by title
        app.lib_filter = "cadre".into();
        assert_eq!(app.lib_rows().len(), 2); // header + game
    }

    #[tokio::test]
    async fn dead_torbox_ids_are_forgotten_for_all_siblings() {
        let magnet = minerva::magnet("696e40e368024e12e4860b93b99f4ef271e4257a");
        let mut all: Vec<Game> = (0..3)
            .map(|i| {
                let mut g = Game::torrent_file(format!("n64-{i}"), format!("Game {i} (USA).zip"), "n64".into(), magnet.clone(), Some(i), 1);
                g.torbox_id = Some(111); // the shared item TorBox has since dropped
                g
            })
            .collect();
        all.push({
            let mut g = Game::new("Zelda.zip".into(), "snes".into(), SourceKind::Magnet, "magnet:?xt=urn:btih:aa".into(), 1);
            g.torbox_id = Some(222);
            g
        });
        let mut app = test_app(all);
        let alive = Item { id: 222, download_present: true, files: vec![crate::torbox::RemoteFile::default()], ..Default::default() };
        // an empty list is ignored
        app.on_msg(Msg::Remote(Kind::Torrent, Ok(Vec::new())));
        assert!(app.lib.games.iter().all(|g| g.torbox_id.is_some()));
        app.on_msg(Msg::Remote(Kind::Torrent, Ok(vec![alive])));
        assert!(app.lib.games.iter().filter(|g| g.system == "n64").all(|g| g.torbox_id.is_none()), "all siblings forget id 111");
        assert_eq!(app.lib.get("n64-0").map(|g| matches!(app.remote.get(&g.id), Some(Remote::Missing))), Some(true));
        assert_eq!(app.lib.games.iter().find(|g| g.system == "snes").and_then(|g| g.torbox_id), Some(222));
    }

    #[tokio::test]
    async fn partial_torbox_games_move_to_real_debrid() {
        let magnet = minerva::magnet("eaaa169ffd32cbf59cbad17d03e3485bd4fdacbe");
        let mut g = Game::torrent_file("psx-1".into(), "Tekken 3 (USA).zip".into(), "psx".into(), magnet, Some(900), 1);
        g.torbox_id = Some(333);
        let mut app = test_app(vec![g]);
        let partial = Item {
            id: 333,
            download_present: true,
            files: vec![
                crate::torbox::RemoteFile { id: 0, name: ".x.parts".into(), ..Default::default() },
                crate::torbox::RemoteFile { id: 1, name: "Chrono Cross (USA) (Disc 1).zip".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        // without Real-Debrid: the "not cached" error
        app.on_msg(Msg::Remote(Kind::Torrent, Ok(vec![partial.clone()])));
        assert!(matches!(app.remote.get("psx-1"), Some(Remote::Error(_))));
        // with Real-Debrid: handed over (its TorBox id dropped)
        app.ctx.rd = Some(crate::realdebrid::RealDebrid::new(reqwest::Client::new(), "k".into()));
        app.on_msg(Msg::Remote(Kind::Torrent, Ok(vec![partial])));
        assert_eq!(app.lib.get("psx-1").and_then(|g| g.torbox_id), None);
        assert!(matches!(app.remote.get("psx-1"), Some(Remote::Sending)));
    }

    #[tokio::test]
    async fn broken_torbox_copies_move_games_to_real_debrid() {
        let h = "6bd38c243f510f9dfa47a3f770324649613c648e";
        let mut g =
            Game::torrent_file("sat-1".into(), "Mortal Kombat II (USA).zip".into(), "saturn".into(), minerva::magnet(h), Some(1235), 1);
        g.torbox_id = Some(147074775);
        let mut app = test_app(vec![g]);
        app.ctx.rd = Some(crate::realdebrid::RealDebrid::new(reqwest::Client::new(), "k".into()));
        assert!(app.minerva_available(h, "Mortal Kombat II (USA).zip"));
        let err = format!("reading TorBox's zip: {}: its zip of this folder is cut off (2.3 GB of 836.4 GB)", jobs::BROKEN_TORBOX_ZIP);
        app.on_msg(Msg::Local("sat-1".into(), LocalState::Failed(err)));
        assert!(!app.minerva_available(h, "Mortal Kombat II (USA).zip"));
        assert_eq!(app.lib.get("sat-1").and_then(|g| g.torbox_id), None);
        assert!(matches!(app.remote.get("sat-1"), Some(Remote::Sending)));
        assert!(!app.local.contains_key("sat-1"));
    }

    #[tokio::test]
    async fn deletes_games_from_disk_after_confirming() {
        let mut dl = Game::torrent_file("ps3-1".into(), "Game (USA).zip".into(), "ps3".into(), "magnet:?xt=urn:btih:aa".into(), None, 1);
        dl.local_files = vec!["ps3/Game (USA).iso".into()];
        let found = Game::local("psx", "psx/Disc (USA).cue".into(), 1);
        let mut found = found;
        found.local_files.push("psx/Disc (USA) (Track 1).bin".into());
        let mut app = test_app(vec![dl, found]);
        let roms = app.ctx.roms_dir.clone();
        for f in ["ps3/Game (USA).iso", "ps3/Game (USA).dkey", "ps3/Other.iso", "psx/Disc (USA).cue", "psx/Disc (USA) (Track 1).bin"] {
            std::fs::create_dir_all(roms.join(f).parent().unwrap()).unwrap();
            std::fs::write(roms.join(f), b"x").unwrap();
        }
        app.lib_cursor = Some(RowKey::Game("ps3-1".into()));
        app.delete_selected_from_disk();
        assert!(roms.join("ps3/Game (USA).iso").exists(), "first X only asks");
        assert!(app.status.contains("press X again to delete 2 file(s)"), "{}", app.status);
        app.delete_selected_from_disk();
        assert!(!roms.join("ps3/Game (USA).iso").exists() && !roms.join("ps3/Game (USA).dkey").exists());
        assert!(roms.join("ps3/Other.iso").exists());
        assert!(app.lib.get("ps3-1").is_some_and(|g| g.local_files.is_empty()), "downloaded games stay in the library");
        // a scanned entry: tracks go too, and so does the entry
        app.lib_cursor = Some(RowKey::Game("local:psx/Disc (USA).cue".into()));
        app.delete_selected_from_disk();
        app.delete_selected_from_disk();
        assert!(!roms.join("psx/Disc (USA) (Track 1).bin").exists());
        assert!(app.lib.get("local:psx/Disc (USA).cue").is_none());
        std::fs::remove_dir_all(&roms).unwrap();
    }

    #[tokio::test]
    async fn library_sorts_by_size() {
        let g = |sys: &str, name: &str, size: u64| Game::local(sys, format!("{sys}/{name}"), size);
        let mut app = test_app(vec![g("nes", "A.zip", 5), g("nes", "B.zip", 50), g("ps2", "C.iso", 100), g("ps2", "D.iso", 900)]);
        let games = |app: &App| -> Vec<String> {
            app.lib_rows().iter().filter_map(|r| if let Row::Game(i) = r { Some(app.lib.games[*i].name.clone()) } else { None }).collect()
        };
        assert_eq!(games(&app), ["A.zip", "B.zip", "C.iso", "D.iso"]);
        app.lib_sort = LibSort::Size;
        assert_eq!(games(&app), ["D.iso", "C.iso", "B.zip", "A.zip"], "biggest system first, biggest game first");
    }

    #[tokio::test]
    async fn bulk_download_runs_two_at_a_time() {
        let mut app = test_app(games("snes", 3));
        let ids: Vec<String> = app.lib.games.iter().map(|g| g.id.clone()).collect();
        for id in &ids {
            app.remote.insert(id.clone(), Remote::Ready);
        }
        app.download_all_ready();
        let running = app.local.values().filter(|s| matches!(s, LocalState::Downloading { .. })).count();
        assert_eq!(running, MAX_DOWNLOADS);
        assert_eq!(app.download_queue.len(), 3 - MAX_DOWNLOADS);
        assert!(app.status.contains("queued 3"), "{}", app.status);
        // a finished download makes room for the next one
        app.on_msg(Msg::Local(ids[0].clone(), LocalState::Failed("x".into())));
        app.pump_downloads();
        assert!(app.download_queue.is_empty());
    }

    #[tokio::test]
    async fn refuses_whole_minerva_folder_magnets() {
        let mut app = test_app(Vec::new());
        let h = "626a5e05f498e435145c97352aa2fecf7e95a668";
        app.put_index(
            minerva::set_key("Redump", "Sony - PlayStation 3"),
            ItemIndex {
                restricted: false,
                files: vec![archive::RomFile {
                    name: "Armored Core 4 (USA).zip".into(),
                    size: 14 << 30,
                    url: None,
                    torrent_index: Some(1),
                    title: None,
                    system: None,
                }],
                system: Some("ps3".into()),
                title: "Minerva · Redump · Sony - PlayStation 3".into(),
                fetched: 0,
                btih: Some(h.into()),
            },
        );
        app.lib_paste = format!("magnet:?xt=urn:btih:{}&dn=ps3", h.to_uppercase());
        app.add_pasted_link();
        assert!(app.lib.games.is_empty());
        assert!(app.status.contains("whole 'Sony - PlayStation 3' folder"), "{}", app.status);
        // an unrelated magnet still goes in
        app.lib_paste = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Some+Game".into();
        app.add_pasted_link();
        assert_eq!(app.lib.games.len(), 1);
    }

    #[tokio::test]
    async fn groups_cap_collapse_and_filter() {
        let mut all = games("snes", 25);
        all.extend(games("gba", 3));
        let mut app = test_app(all);
        let s = shape(&app);
        // SNES before GBA (SYSTEMS order), capped at 20 + "5 more"
        assert_eq!(s[0], "H snes 25");
        assert_eq!(s.iter().filter(|r| *r == "g").count(), 23);
        assert!(s.contains(&"M snes 5".to_string()));
        assert_eq!(s[22], "H gba 3");

        // a filter shows every match, no cap
        app.lib_filter = "snes".into();
        assert_eq!(shape(&app).iter().filter(|r| *r == "g").count(), 25);
        app.lib_filter.clear();

        // collapse SNES from one of its games: cursor jumps to the header
        app.lib_cursor = Some(RowKey::Game(app.lib.games[3].id.clone()));
        app.toggle_current_group();
        assert_eq!(app.lib_cursor, Some(RowKey::Header("snes".into())));
        assert_eq!(&shape(&app)[..2], &["H snes 25 collapsed".to_string(), "H gba 3".to_string()]);

        // Enter on the header expands it again; Space opens its menu instead
        app.tab = Tab::Library;
        app.press(KeyCode::Enter);
        assert!(app.menu.is_none());
        assert_eq!(shape(&app)[0], "H snes 25");
        app.lib_cursor = Some(RowKey::Header("snes".into()));
        app.press(KeyCode::Char(' '));
        assert_eq!(app.menu.as_ref().map(|m| m.items[0].1), Some(Act::Fold));
        app.press(KeyCode::Esc);

        // Enter on "… more" filters to that system
        app.lib_cursor = Some(RowKey::More("snes".into()));
        app.press(KeyCode::Enter);
        assert_eq!(app.lib_filter, "snes");
        assert!(matches!(app.lib_cursor, Some(RowKey::Game(_))));

        // collapse all / expand all
        app.lib_filter.clear();
        app.set_all_collapsed(true);
        assert_eq!(shape(&app), vec!["H snes 25 collapsed", "H gba 3 collapsed"]);
        app.set_all_collapsed(false);
        assert_eq!(shape(&app)[0], "H snes 25");
    }

    fn menu_acts(app: &App) -> Vec<Act> {
        app.menu.as_ref().map(|m| m.items.iter().map(|i| i.1).collect()).unwrap_or_default()
    }

    #[tokio::test]
    async fn pad_menus() {
        let mut app = test_app(games("snes", 2));
        app.tab = Tab::Library;
        app.lib_cursor = Some(RowKey::Game(app.lib.games[0].id.clone()));

        // A downloads (no TorBox key here: direct); B opens the game's actions, Download first
        app.on_pad(Pad::A);
        assert!(app.menu.is_none());
        app.local.clear();
        app.on_pad(Pad::B);
        assert_eq!(menu_acts(&app), vec![Act::Download, Act::DownloadDirect, Act::System, Act::Remove]);
        // ←/→ on the System row changes the game's system in place
        app.on_pad(Pad::Down);
        app.on_pad(Pad::Down);
        app.on_pad(Pad::Right);
        assert_ne!(app.lib.games[0].system, "snes");
        assert!(app.menu.as_ref().unwrap().items[2].0.starts_with("System: "));
        app.on_pad(Pad::B);
        assert!(app.menu.is_none());

        // Y: the tab menu; B closes; LB/RB switch tabs
        app.on_pad(Pad::Y);
        assert_eq!(menu_acts(&app)[0], Act::Search);
        assert_eq!(*menu_acts(&app).last().unwrap(), Act::Quit);
        app.on_pad(Pad::B);
        app.tex.loading = true; // no network fetch of the texture lists from a test
        app.on_pad(Pad::RB);
        assert!(app.tab == Tab::Textures);
        app.on_pad(Pad::RB);
        assert!(app.tab == Tab::Sources);
        app.on_pad(Pad::RB);
        assert!(app.tab == Tab::Config);
        app.on_pad(Pad::RB);
        assert!(app.tab == Tab::Search, "wraps around");
        app.on_pad(Pad::LB);
        assert!(app.tab == Tab::Config);
        app.on_pad(Pad::LB);
        assert!(app.tab == Tab::Sources);
        app.on_pad(Pad::LB);
        assert!(app.tab == Tab::Textures);
        app.on_pad(Pad::LB);
        assert!(app.tab == Tab::Library);

        // X starts typing; letter-free buttons still work: A finishes, LB leaves and switches tab
        app.on_pad(Pad::X);
        assert!(app.editing);
        app.on_pad(Pad::A);
        assert!(!app.editing);
        app.on_pad(Pad::X);
        app.on_pad(Pad::LB);
        assert!(!app.editing);
        assert!(app.tab == Tab::Search);

        // Quit from the menu
        app.on_pad(Pad::Y);
        let n = menu_acts(&app).len();
        for _ in 0..n - 1 {
            app.on_pad(Pad::Down);
        }
        app.on_pad(Pad::A);
        assert!(app.quit);
    }

    #[tokio::test]
    async fn textures_tab_lists_and_toggles_packs() {
        use crate::textures::{Build, Catalog, Installed, Pack};
        let mut gs = games("gc", 1);
        gs[0].name = "Luigi's Mansion (USA).rvz".into();
        gs.extend(games("n64", 1));
        let mut app = test_app(gs);
        app.tex.dirs.retroarch_opts = None;
        let file = app.lib.games[0].local_paths(&app.ctx.roms_dir)[0].clone();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut rvz = vec![0u8; 0x100];
        rvz[..4].copy_from_slice(b"RVZ\x01");
        rvz[0x58..0x5e].copy_from_slice(b"GLME01");
        std::fs::write(&file, &rvz).unwrap();
        app.refresh_presence();

        let pack = |key: &str, author: &str, url: &str| Pack {
            key: key.into(),
            system: "gc".into(),
            game: "Luigi's Mansion".into(),
            game_id: Some("GLM".into()),
            author: author.into(),
            page: None,
            builds: vec![Build { label: "DDS".into(), url: url.into(), size: 608 << 20 }],
            sources: vec!["EmulationKing".into()],
            summary: "Around 3,500 textures.".into(),
            ..Pack::default()
        };
        let other = Pack {
            system: "gc".into(),
            game: "Metroid Prime".into(),
            game_id: Some("GM8E01".into()),
            ..pack("gc/mp/x", "x", "https://mega.nz/x")
        };
        let packs = vec![
            pack("gc/lm/cool", "coolhardy100", "https://files.emulationking.com/lm.7z"),
            pack("gc/lm/web", "someone", "https://www.patreon.com/x"),
            other,
        ];
        crate::textures::save_catalog(&app.cache_dir, &Catalog { fetched: library::now_secs(), packs }).unwrap();

        app.press(KeyCode::Char('3'));
        assert!(app.tab == Tab::Textures);
        assert!(!app.tex.loading, "a fresh cached list isn't fetched again");
        let shape: Vec<String> = app
            .tex_rows()
            .iter()
            .map(|r| match r {
                TexRow::System { system, with_packs, on_disk } => format!("{system} {with_packs}/{on_disk}"),
                TexRow::Game(g) => app.lib.games[*g].name.clone(),
                TexRow::Pack(_, p) => app.tex.pack(*p).unwrap().author.clone(),
            })
            .collect();
        assert_eq!(shape, ["gc 1/1", "Luigi's Mansion (USA).rvz", "coolhardy100", "someone", "n64 0/0"]);

        // the pack's menu offers its build; a web-only pack offers its page
        app.press(KeyCode::Down);
        app.press(KeyCode::Down);
        app.press(KeyCode::Char(' '));
        let labels: Vec<String> = app.menu.as_ref().unwrap().items.iter().map(|i| i.0.clone()).collect();
        assert_eq!(labels, ["Install DDS · 608.0 MB", "Close"]);
        app.press(KeyCode::Esc);
        app.press(KeyCode::Down);
        app.press(KeyCode::Char(' '));
        assert_eq!(app.menu.as_ref().unwrap().items[0].0, "Open DDS in a browser");
        app.press(KeyCode::Esc);
        app.press(KeyCode::Up);

        // installed (off): Enter turns it on, again turns it off
        let d = app.tex.dirs.clone();
        let store = d.root.join("romgetter/gc_lm_cool/GLM");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("tex1.png"), b"x").unwrap();
        let inst =
            Installed { system: "gc".into(), folder: "GLM".into(), build: "DDS".into(), enabled: false, size: 1, cache_files: vec![] };
        app.tex.installed.insert("gc/lm/cool".into(), inst);
        app.press(KeyCode::Enter);
        assert!(d.live("gc").join("GLM/tex1.png").exists(), "{}", app.status);
        app.press(KeyCode::Enter);
        assert!(!d.live("gc").join("GLM").exists());
        assert!(store.join("tex1.png").exists());
        let _ = std::fs::remove_dir_all(&d.root);
    }

    #[tokio::test]
    async fn sizes_count_only_whats_on_disk() {
        let mut gs = games("gba", 2);
        gs[0].size = 1000;
        gs[1].size = 5000;
        let mut app = test_app(gs);
        std::fs::create_dir_all(app.ctx.roms_dir.join("gba")).unwrap();
        let file = app.lib.games[0].local_paths(&app.ctx.roms_dir)[0].clone();
        std::fs::write(&file, b"rom").unwrap();
        app.refresh_presence();
        let (a, b) = (app.lib.games[0].clone(), app.lib.games[1].clone());
        assert_eq!(app.size_on_disk(&a), 3, "the file's real size, not the source's");
        assert_eq!(app.size_on_disk(&b), 0, "not downloaded");
    }

    #[tokio::test]
    async fn menu_delete_asks_first() {
        let mut app = test_app(games("gba", 1));
        let dir = app.ctx.roms_dir.join("gba");
        std::fs::create_dir_all(&dir).unwrap();
        let g = app.lib.games[0].clone();
        let file = g.local_paths(&app.ctx.roms_dir)[0].clone();
        std::fs::write(&file, b"rom").unwrap();
        app.refresh_presence();
        app.tab = Tab::Library;
        app.lib_cursor = Some(RowKey::Game(g.id));

        app.press(KeyCode::Char(' '));
        let acts = menu_acts(&app);
        assert!(!acts.contains(&Act::Download), "already on disk: {acts:?}");
        let del = acts.iter().position(|a| *a == Act::DeleteAsk).unwrap();
        app.menu.as_mut().unwrap().sel = del;
        app.press(KeyCode::Enter);
        // a confirmation menu, Cancel first
        assert_eq!(menu_acts(&app), vec![Act::Close, Act::DeleteYes]);
        assert!(app.menu.as_ref().unwrap().title.starts_with("Delete 1 file(s)"));
        app.press(KeyCode::Enter); // Cancel
        assert!(file.exists());

        app.press(KeyCode::Char(' '));
        let del = menu_acts(&app).iter().position(|a| *a == Act::DeleteAsk).unwrap();
        app.menu.as_mut().unwrap().sel = del;
        app.press(KeyCode::Enter);
        app.press(KeyCode::Down);
        app.press(KeyCode::Enter);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(app.ctx.roms_dir.parent().unwrap());
    }

    /// A Vimm game picks up its file from the downloads folder once the file stops growing,
    /// matching by title (and region), never a different game's file.
    #[tokio::test]
    async fn vimm_download_is_picked_up() {
        let mut app = test_app(vec![]);
        let dl = app.ctx.roms_dir.parent().unwrap().join("dl-vimm");
        let _ = std::fs::remove_dir_all(&dl);
        std::fs::create_dir_all(&dl).unwrap();
        app.downloads_dir = dl.clone();
        for (name, id) in [("Chrono Trigger (USA)", 1127), ("Chrono Trigger (Japan)", 41359), ("Super Metroid (USA)", 1)] {
            app.lib.games.push(Game::new(name.into(), "snes".into(), SourceKind::Vimm, vimm::page_url(id), 0));
        }
        std::fs::write(dl.join("Chrono Trigger (USA).zip"), b"rom").unwrap();
        std::fs::write(dl.join("Some Other Thing.pdf"), b"pdf").unwrap();
        std::fs::write(dl.join("Super Metroid (USA).zip"), b"").unwrap(); // Firefox placeholder
        std::fs::write(dl.join("Super Metroid (USA).zip.part"), b"half").unwrap();

        app.watch_downloads(); // first look: sizes noted, nothing taken yet
        assert!(app.local.is_empty());
        app.last_dl_scan = None;
        app.watch_downloads();
        let busy: Vec<&str> = app.local.keys().map(String::as_str).collect();
        assert_eq!(busy, vec![vimm::page_url(1127).as_str()]);
        assert!(app.dl_claimed.contains(&dl.join("Chrono Trigger (USA).zip")));
        let _ = std::fs::remove_dir_all(&dl);
    }

    /// The help line follows whichever input was used last; on-screen-keyboard typing in a
    /// box the pad opened doesn't count as the keyboard.
    #[tokio::test]
    async fn help_follows_last_input() {
        let mut app = test_app(games("snes", 5));
        app.tab = Tab::Library;
        app.editing = false;
        app.on_pad(Pad::Down);
        assert!(app.pad);
        app.on_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert!(!app.pad);
        app.on_pad(Pad::X);
        assert!(app.pad && app.editing);
        app.on_event(Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(app.pad, "Steam keyboard typing keeps button hints");
    }

    /// The item menu draws as a popup over the list.
    #[tokio::test]
    async fn menu_renders() {
        let mut app = test_app(games("snes", 5));
        app.tab = Tab::Library;
        app.editing = false;
        app.pad = true;
        app.lib_cursor = Some(RowKey::Game(app.lib.games[1].id.clone()));
        app.on_pad(Pad::B);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let text: String =
            (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n").collect();
        if std::env::var_os("SHOW").is_some() {
            println!("{text}");
        }
        assert!(text.contains("▶ Download"), "{text}");
        assert!(text.contains("System: ◂ "), "{text}");
        assert!(text.contains("[A] choose"), "{text}");
    }

    /// Renders the Library into an off-screen terminal.
    #[tokio::test]
    async fn library_renders() {
        let mut all = games("snes", 25);
        all.extend(games("gba", 3));
        let mut app = test_app(all);
        app.tab = Tab::Library;
        app.editing = false;
        app.collapsed.insert("gba".into());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 32)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .chunks(110)
            .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect::<String>().trim_end().to_string() + "\n")
            .collect();
        if std::env::var("SHOW").is_ok() {
            println!("{text}");
        }
        assert!(text.contains("▾ Nintendo - SNES"));
        assert!(text.contains("… 5 more"));
        assert!(text.contains("▸ Nintendo - Game Boy Advance"));
    }

    #[tokio::test]
    async fn big_libraries_start_collapsed() {
        let mut all = games("snes", 30);
        all.extend(games("n64", 15));
        let app = test_app(all);
        assert_eq!(shape(&app), vec!["H snes 30 collapsed", "H n64 15 collapsed"]);
        assert_eq!(app.lib_cursor, Some(RowKey::Header("snes".into())));
    }

    #[test]
    fn live_rom_plausibility() {
        assert!(!plausible_live_rom("Squid_Vs_Octo.zip", 2_600_000, "switch"));
        assert!(plausible_live_rom("0100F8F0000A2000.nsp", 1_000, "switch")); // extension says ROM
        assert!(plausible_live_rom("Game (USA).zip", 3_100_000_000, "switch"));
        assert!(plausible_live_rom("Tetris (World).zip", 30_000, "gb")); // cartridge ROMs are small
    }

    #[test]
    fn ranking_prefers_clean_usa() {
        let mut names = [
            "Super Mario World (Japan).zip",
            "Super Mario World (USA) (Beta).zip",
            "Super Mario World (Europe).zip",
            "Super Mario World (USA).zip",
        ];
        names.sort_by_key(|n| rank(n));
        assert_eq!(names[0], "Super Mario World (USA).zip");
        assert_eq!(names[3], "Super Mario World (USA) (Beta).zip");
    }
}
