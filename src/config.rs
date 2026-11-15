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
    /// Search Minerva (torrents of No-Intro/Redump, usually cached on TorBox).
    #[serde(default = "yes")]
    pub use_minerva: bool,
    /// Search the built-in No-Intro/Redump sets (see sources.rs).
    #[serde(default = "yes")]
    pub use_default_sets: bool,
    /// Search Vimm's Lair (downloaded through a browser). A last resort, so off by
    /// default; the Search tab's menu toggles it.
    #[serde(default)]
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
            use_vimm: false,
            browser: None,
            downloads_dir: None,
            extra_sets: Vec::new(),
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
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
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
