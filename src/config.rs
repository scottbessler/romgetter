use anyhow::{Context, Result};
use directories::{BaseDirs, ProjectDirs};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    /// archive.org item identifier
    pub item: String,
    #[serde(default)]
    pub title: String,
    /// RetroDECK roms sub-folder for this set's files; empty = detect.
    #[serde(default)]
    pub system: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// TorBox API key. The TORBOX_API_KEY env var overrides this.
    #[serde(default)]
    pub torbox_api_key: Option<String>,
    /// Real-Debrid API key (REAL_DEBRID_API_KEY overrides). Used for Minerva games TorBox
    /// can't deliver: Real-Debrid can download one file out of a huge folder torrent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub real_debrid_api_key: Option<String>,
    /// RetroDECK roms folder (contains snes/, gba/, psx/, ...).
    pub roms_dir: PathBuf,
    /// Seconds between TorBox status polls.
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
    // Which groups of sets exist at all: Minerva, the built-in archive.org sets, ROM-set
    // torrents in the TorBox account, the IF Archive. Always on from a config file: an old
    // `use_minerva = false` etc. becomes the group's sets switched off in the Sources tab
    // (`groups_off`, applied at startup); only tests leave groups out.
    #[serde(default = "yes", skip_serializing)]
    pub use_minerva: bool,
    #[serde(default = "yes", skip_serializing)]
    pub use_default_sets: bool,
    #[serde(default = "yes", skip_serializing)]
    pub use_torbox_zips: bool,
    #[serde(default = "yes", skip_serializing)]
    pub use_ifarchive: bool,
    /// Groups an old `use_*` switch had off (see above), to switch off in the Sources tab.
    #[serde(skip)]
    pub groups_off: Vec<Group>,
    /// Old switch for Vimm's Lair; now `[sources] vimm = true` (read once, then dropped).
    #[serde(default, skip_serializing)]
    pub use_vimm: bool,
    /// Browser command for Vimm pages (the URL is appended), e.g. "flatpak run org.mozilla.firefox".
    /// Default: Firefox or Chrome if installed, else the system default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// Where the browser saves downloads; watched for Vimm games. Default: ~/Downloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads_dir: Option<PathBuf>,
    /// Extra archive.org sets to index for Search, on top of the built-in ones.
    #[serde(default, alias = "collections")]
    pub extra_sets: Vec<Collection>,
    /// archive.org session from `romgetter archive-login` (the password isn't kept).
    /// Unlocks "restricted" sets, which then download directly instead of via TorBox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_login: Option<ArchiveLogin>,
    /// Sources switched on or off in the Sources tab, over their defaults (all on but
    /// Vimm's Lair): a set's key ("minerva:Redump/Sony - PlayStation 2", "torbox:<hash>",
    /// an archive.org identifier), "archive.org" (live search) or "vimm".
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sources: std::collections::BTreeMap<String, bool>,
}

/// Groups of sets the old `use_*` switches covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Minerva,
    DefaultSets,
    TorboxZips,
    IfArchive,
}

/// Live-search sources (Sources tab keys).
pub const LIVE_ARCHIVE: &str = "archive.org";
pub const LIVE_VIMM: &str = "vimm";

/// archive.org's login cookies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveLogin {
    /// `logged-in-user` (the account's email).
    pub user: String,
    /// `logged-in-sig`.
    pub sig: String,
}

fn yes() -> bool {
    true
}

fn default_poll() -> u64 {
    5
}

fn default_roms_dir() -> PathBuf {
    BaseDirs::new().map(|b| b.home_dir().join("retrodeck").join("roms")).unwrap_or_else(|| PathBuf::from("retrodeck/roms"))
}

impl Default for Config {
    fn default() -> Self {
        Self {
            torbox_api_key: None,
            real_debrid_api_key: None,
            roms_dir: default_roms_dir(),
            poll_secs: default_poll(),
            use_minerva: true,
            use_default_sets: true,
            use_torbox_zips: true,
            use_ifarchive: true,
            groups_off: Vec::new(),
            use_vimm: false,
            browser: None,
            downloads_dir: None,
            extra_sets: Vec::new(),
            archive_login: None,
            sources: std::collections::BTreeMap::new(),
        }
    }
}

pub struct Paths {
    pub config_file: PathBuf,
    pub cache_dir: PathBuf,
    pub library_file: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "romgetter").context("no home directory")?;
        let config_file = std::env::var_os("ROMGETTER_CONFIG").map(PathBuf::from).unwrap_or_else(|| dirs.config_dir().join("config.toml"));
        let library_file = std::env::var_os("ROMGETTER_LIBRARY").map(PathBuf::from).unwrap_or_else(|| dirs.data_dir().join("library.json"));
        Ok(Self { config_file, cache_dir: dirs.cache_dir().to_path_buf(), library_file })
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            let cfg = Config::default();
            cfg.save(path)?;
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        for (on, group) in [
            (&mut cfg.use_minerva, Group::Minerva),
            (&mut cfg.use_default_sets, Group::DefaultSets),
            (&mut cfg.use_torbox_zips, Group::TorboxZips),
            (&mut cfg.use_ifarchive, Group::IfArchive),
        ] {
            if !std::mem::replace(on, true) {
                cfg.groups_off.push(group);
            }
        }
        if std::mem::take(&mut cfg.use_vimm) {
            cfg.sources.entry(LIVE_VIMM.into()).or_insert(true);
        }
        Ok(cfg)
    }

    /// Whether a source (see `sources`) is on.
    pub fn source_on(&self, key: &str) -> bool {
        self.sources.get(key).copied().unwrap_or(key != LIVE_VIMM)
    }

    /// Turn a source on or off; a source back at its default leaves the table.
    pub fn set_source(&mut self, key: &str, on: bool) {
        if on == (key != LIVE_VIMM) {
            self.sources.remove(key);
        } else {
            self.sources.insert(key.to_string(), on);
        }
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn rd_key(&self) -> Option<String> {
        std::env::var("REAL_DEBRID_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| self.real_debrid_api_key.clone().filter(|k| !k.trim().is_empty()))
    }

    pub fn api_key(&self) -> Option<String> {
        std::env::var("TORBOX_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| self.torbox_api_key.clone().filter(|k| !k.trim().is_empty()))
    }
}
