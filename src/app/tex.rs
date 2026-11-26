//! The Textures tab: texture packs for the games on disk, one row per game and one row
//! per pack under it (shown when the game is chosen).

use super::{Act, App, Menu, Msg};
use crate::library::now_secs;
use crate::textures::{self, Catalog, Dirs, Installed, Pack, Reach, TexState};
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};


/// Catalog packs name their system after the sheet's tab; the library uses its own folder
/// names (`systems.rs`). Keep only the ones the library knows about.
fn known_system(s: &str) -> Option<&'static str> {
    Some(match s {
        "gc" => "gc",
        "n64" => "n64",
        "ps2" => "ps2",
        "wii" => "wii",
        "psx" => "psx",
        "psp" => "psp",
        "psvita" => "psvita",
        "n3ds" => "n3ds",
        "switch" => "switch",
        "dreamcast" => "dreamcast",
        _ => return None,
    })
}

pub struct TexTab {
    pub catalog: Option<Catalog>,
    pub loading: bool,
    /// Installed packs by key.
    pub installed: HashMap<String, Installed>,
    /// Downloads/installs in progress (or failed) by pack key.
    pub jobs: HashMap<String, TexState>,
    pub cursor: Option<TexKey>,
    pub state: ListState,
    /// Library game id → disc ID / ROM name read from its file (None: unreadable).
    tags: HashMap<String, Option<String>>,
    /// Games on disk with packs, in display order: (system, library game id, pack indices).
    pub matches: Vec<(&'static str, String, Vec<usize>)>,
    /// Games on disk per system (with or without packs).
    pub on_disk: HashMap<&'static str, usize>,
    /// Systems whose games are hidden (`▸`); every other system shows its games.
    pub collapsed: HashSet<&'static str>,
    /// Game ids whose packs are shown (`▾`) under the game row.
    pub expanded: HashSet<String>,
    pub dirs: Dirs,
    opened: bool,
}

/// What the cursor is on; stable across refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TexKey {
    System(&'static str),
    Game(String),
    /// (game id, pack key)
    Pack(String, String),
}

pub enum TexRow {
    System {
        system: &'static str,
        with_packs: usize,
        on_disk: usize,
        /// Total packs across this system's games on disk.
        packs: usize,
        collapsed: bool,
    },
    /// A game on disk: index into `lib.games`, how many packs fit it, how many are enabled.
    Game { game: usize, packs: usize, enabled: usize },
    /// (index into `lib.games`, index into the catalog)
    Pack(usize, usize),
}

impl TexTab {
    pub fn new(roms_dir: &std::path::Path) -> Self {
        let dirs = Dirs::for_roms(roms_dir);
        Self {
            catalog: None,
            loading: false,
            installed: textures::load_installed(&dirs),
            jobs: HashMap::new(),
            cursor: None,
            state: ListState::default(),
            tags: HashMap::new(),
            matches: Vec::new(),
            on_disk: HashMap::new(),
            collapsed: HashSet::new(),
            expanded: HashSet::new(),
            dirs,
            opened: false,
        }
    }

    pub fn pack(&self, i: usize) -> Option<&Pack> {
        self.catalog.as_ref()?.packs.get(i)
    }
}

impl App {
    /// The tab was opened: load (or fetch) the catalog and match it to the games on disk.
    pub(super) fn tex_open(&mut self) {
        if !self.tex.opened {
            self.tex.opened = true;
            self.tex.catalog = textures::load_catalog(&self.cache_dir);
            let stale = self.tex.catalog.as_ref().is_none_or(|c| now_secs().saturating_sub(c.fetched) > textures::REFRESH.as_secs());
            if stale {
                self.tex_fetch();
            }
        }
        self.tex_match();
    }

    pub(super) fn tex_fetch(&mut self) {
        if self.tex.loading {
            return;
        }
        self.tex.loading = true;
        self.status = "fetching texture pack lists (EmulationKing, rapka, putachi, the texture sheet)…".into();
        let (http, tx) = (self.ctx.http.clone(), self.tx.clone());
        tokio::spawn(async move {
            let _ = tx.send(Msg::TexCatalog(textures::fetch_catalog(&http).await));
        });
    }

    /// Recompute which packs fit which games on disk.
    pub(super) fn tex_match(&mut self) {
        let roms = self.ctx.roms_dir.clone();
        let mut matches = Vec::new();
        let mut on_disk: HashMap<&'static str, usize> = HashMap::new();
        let mut systems: Vec<&'static str> = textures::SYSTEMS.to_vec();
        if let Some(c) = &self.tex.catalog {
            for p in &c.packs {
                if let Some(sys) = known_system(&p.system)
                    && !systems.contains(&sys)
                {
                    systems.push(sys);
                }
            }
        }
        for system in systems {
            let mut games: Vec<&crate::library::Game> =
                self.lib.games.iter().filter(|g| g.system == system && self.present.contains(&g.id)).collect();
            games.sort_by_cached_key(|g| g.name.to_lowercase());
            on_disk.insert(system, games.len());
            for g in games {
                let tag = self
                    .tex
                    .tags
                    .entry(g.id.clone())
                    .or_insert_with(|| g.local_paths(&roms).first().and_then(|p| textures::game_tag(system, p)))
                    .clone();
                let packs: Vec<usize> = self.tex.catalog.as_ref().map_or_else(Vec::new, |c| {
                    c.packs.iter().enumerate().filter(|(_, p)| textures::fits(p, system, &g.name, tag.as_deref())).map(|(i, _)| i).collect()
                });
                if !packs.is_empty() {
                    matches.push((system, g.id.clone(), packs));
                }
            }
        }
        self.tex.matches = matches;
        self.tex.on_disk = on_disk;
        if self.tex.cursor.is_none() {
            self.tex.cursor = self.tex_keys().into_iter().next();
        }
    }

    pub fn tex_rows(&self) -> Vec<TexRow> {
        let mut rows = Vec::new();
        let mut systems: Vec<&'static str> = textures::SYSTEMS.to_vec();
        if let Some(c) = &self.tex.catalog {
            for p in &c.packs {
                if let Some(sys) = known_system(&p.system)
                    && !systems.contains(&sys)
                {
                    systems.push(sys);
                }
            }
        }
        for system in systems {
            let games: Vec<_> = self.tex.matches.iter().filter(|m| m.0 == system).collect();
            let pack_total: usize = games.iter().map(|g| g.2.len()).sum();
            let with_packs = games.len();
            // Systems with no on-disk game that has a pack are left out, except the two the tab
            // has always shown (so "0 of N on disk" still tells you nothing's available).
            if with_packs == 0 && !textures::SYSTEMS.contains(&system) {
                continue;
            }
            let collapsed = self.tex.collapsed.contains(system);
            rows.push(TexRow::System {
                system,
                with_packs,
                on_disk: self.tex.on_disk.get(system).copied().unwrap_or(0),
                packs: pack_total,
                collapsed,
            });
            if collapsed {
                continue;
            }
            for (_, id, packs) in games {
                let Some(gi) = self.lib.games.iter().position(|g| &g.id == id) else { continue };
                let enabled =
                    packs.iter().filter(|&&pi| self.tex.pack(pi).and_then(|p| self.tex.installed.get(&p.key)).is_some_and(|i| i.enabled)).count();
                rows.push(TexRow::Game { game: gi, packs: packs.len(), enabled });
                if self.tex.expanded.contains(id) {
                    rows.extend(packs.iter().map(|&p| TexRow::Pack(gi, p)));
                }
            }
        }
        rows
    }

    fn tex_key(&self, r: &TexRow) -> Option<TexKey> {
        Some(match r {
            TexRow::System { system, .. } => TexKey::System(system),
            TexRow::Game { game, .. } => TexKey::Game(self.lib.games[*game].id.clone()),
            TexRow::Pack(g, p) => TexKey::Pack(self.lib.games[*g].id.clone(), self.tex.pack(*p)?.key.clone()),
        })
    }

    fn tex_keys(&self) -> Vec<TexKey> {
        self.tex_rows().iter().filter_map(|r| self.tex_key(r)).collect()
    }

    /// Row index of the cursor, for the list widget.
    pub fn tex_selected(&self) -> Option<usize> {
        let cur = self.tex.cursor.as_ref()?;
        self.tex_rows().iter().position(|r| self.tex_key(r).as_ref() == Some(cur))
    }

    pub(super) fn tex_move(&mut self, delta: isize) {
        let keys = self.tex_keys();
        if keys.is_empty() {
            return;
        }
        let cur = self.tex.cursor.as_ref().and_then(|c| keys.iter().position(|k| k == c)).unwrap_or(0) as isize;
        self.tex.cursor = Some(keys[(cur + delta).clamp(0, keys.len() as isize - 1) as usize].clone());
    }

    /// Collapse or expand a system, moving the cursor to its header when collapsing.
    pub(super) fn tex_set_collapsed(&mut self, system: &'static str, collapse: bool) {
        if collapse {
            self.tex.collapsed.insert(system);
            self.tex.cursor = Some(TexKey::System(system));
        } else {
            self.tex.collapsed.remove(&system);
        }
    }

    /// Left/Right: collapse or expand the system under the cursor (only on a header row).
    pub(super) fn tex_fold_here(&mut self, collapse: bool) {
        if let Some(TexKey::System(system)) = self.tex.cursor.clone() {
            self.tex_set_collapsed(system, collapse);
        }
    }

    /// The pack under the cursor, and the game it's shown under.
    fn tex_selected_pack(&self) -> Option<(String, Pack)> {
        let Some(TexKey::Pack(game, key)) = &self.tex.cursor else { return None };
        let p = self.tex.catalog.as_ref()?.packs.iter().find(|p| &p.key == key)?;
        Some((game.clone(), p.clone()))
    }

    /// Enter: toggle a system's games or a game's packs; on a pack, install it (its default
    /// build) or turn an installed one on/off. Packs only on a web page open in the browser.
    pub(super) fn tex_activate(&mut self) {
        match self.tex.cursor.clone() {
            Some(TexKey::System(system)) => {
                let collapse = !self.tex.collapsed.contains(system);
                return self.tex_set_collapsed(system, collapse);
            }
            Some(TexKey::Game(id)) => {
                if !self.tex.expanded.remove(&id) {
                    self.tex.expanded.insert(id);
                }
                return;
            }
            _ => {}
        }
        let Some((_, p)) = self.tex_selected_pack() else { return self.tex_item_menu() };
        if let Some(s) = self.tex.jobs.get(&p.key)
            && !matches!(s, TexState::Failed(_))
        {
            self.status = format!("{} is already on its way", p.game);
            return;
        }
        if self.tex.installed.contains_key(&p.key) {
            return self.tex_toggle(&p.key);
        }
        match p.default_build() {
            Some(b) => self.tex_install(b),
            None => self.tex_open_url(0),
        }
    }

    /// Space / B: the pack's actions (one install entry per build).
    pub(super) fn tex_item_menu(&mut self) {
        let Some((_, p)) = self.tex_selected_pack() else { return };
        let mut items = Vec::new();
        if let Some(i) = self.tex.installed.get(&p.key) {
            items.push((if i.enabled { "Turn off" } else { "Turn on" }.to_string(), Act::TexToggle));
            items.push((format!("Delete from disk ({})", crate::ui::human(i.size)), Act::TexRemove));
        } else {
            for (i, b) in p.builds.iter().enumerate() {
                let size = if b.size > 0 { format!(" · {}", crate::ui::human(b.size)) } else { String::new() };
                if textures::reach(&b.url) == Reach::Browser {
                    items.push((format!("Open {} in a browser", b.label), Act::TexOpenUrl(i)));
                } else {
                    items.push((format!("Install {}{size}", b.label), Act::TexInstall(i)));
                }
            }
        }
        if p.page.is_some() {
            items.push(("Open the pack's page in a browser".into(), Act::TexPage));
        }
        items.push(("Close".into(), Act::Close));
        self.menu = Some(Menu { title: format!("{} · {}", p.game, p.author), items, sel: 0 });
    }

    pub(super) fn tex_run(&mut self, act: Act) {
        match act {
            Act::TexInstall(b) => self.tex_install(b),
            Act::TexToggle => {
                if let Some((_, p)) = self.tex_selected_pack() {
                    self.tex_toggle(&p.key);
                }
            }
            Act::TexRemove => {
                let Some((_, p)) = self.tex_selected_pack() else { return };
                self.status = match textures::remove(&self.tex.dirs, &p.key, &mut self.tex.installed) {
                    Ok(()) => format!("deleted {}'s {} pack", p.author, p.game),
                    Err(e) => format!("couldn't delete the pack: {e:#}"),
                };
            }
            Act::TexOpenUrl(b) => self.tex_open_url(b),
            Act::TexPage => {
                if let Some(page) = self.tex_selected_pack().and_then(|(_, p)| p.page) {
                    self.tex_browse(&page);
                }
            }
            Act::TexRefresh => self.tex_fetch(),
            _ => {}
        }
    }

    fn tex_toggle(&mut self, key: &str) {
        let Some(on) = self.tex.installed.get(key).map(|i| i.enabled) else { return };
        let d = self.tex.dirs.clone();
        let res = if on { textures::disable(&d, key, &mut self.tex.installed) } else { textures::enable(&d, key, &mut self.tex.installed) };
        self.status = match res {
            Ok(()) if on => "texture pack turned off".into(),
            Ok(()) => "texture pack turned on (takes effect next time the game starts)".into(),
            Err(e) => format!("{e:#}"),
        };
    }

    fn tex_install(&mut self, build: usize) {
        let Some((game_id, p)) = self.tex_selected_pack() else { return };
        let Some(b) = p.builds.get(build).cloned() else { return };
        if textures::reach(&b.url) == Reach::Browser {
            return self.tex_open_url(build);
        }
        let folder = self.tex.tags.get(&game_id).cloned().flatten();
        let (key, tx) = (p.key.clone(), self.tx.clone());
        let via = match (textures::reach(&b.url), self.ctx.torbox.is_some(), self.ctx.rd.is_some()) {
            (Reach::Debrid, true, _) => "TorBox",
            (Reach::Debrid, false, true) => "Real-Debrid",
            _ => "a direct download",
        };
        self.status = format!("getting {}'s {} pack ({}) via {via}…", p.author, p.game, b.label);
        self.tex.jobs.insert(key.clone(), TexState::Remote { what: "starting".into(), progress: 0.0 });
        let (d, ctx) = (self.tex.dirs.clone(), self.ctx.clone());
        tokio::spawn(textures::get(p, b, folder, d, ctx, move |s| {
            let _ = tx.send(Msg::Tex(key.clone(), s));
        }));
    }

    fn tex_open_url(&mut self, build: usize) {
        if let Some(url) = self.tex_selected_pack().and_then(|(_, p)| p.builds.get(build).map(|b| b.url.clone())) {
            self.tex_browse(&url);
        }
    }

    fn tex_browse(&mut self, url: &str) {
        self.status = match self.open_url(url) {
            Ok(b) => format!("opened in {b}: {url}"),
            Err(e) => format!("{e:#} · open {url} yourself"),
        };
    }

    pub(super) fn on_tex(&mut self, key: String, s: TexState) {
        match s {
            TexState::Done(i) => {
                self.tex.jobs.remove(&key);
                self.tex.installed = textures::load_installed(&self.tex.dirs);
                let name = self.tex.catalog.as_ref().and_then(|c| c.packs.iter().find(|p| p.key == key)).map(|p| p.game.clone());
                self.status = format!(
                    "installed and turned on the {} pack → {} ({})",
                    name.unwrap_or_default(),
                    self.tex.dirs.live(&i.system).join(&i.folder).display(),
                    crate::ui::human(i.size)
                );
            }
            TexState::Failed(ref m) => {
                self.status = format!("texture pack failed: {m}");
                self.tex.jobs.insert(key, s);
            }
            _ => {
                self.tex.jobs.insert(key, s);
            }
        }
    }

    pub(super) fn on_tex_catalog(&mut self, r: anyhow::Result<Catalog>) {
        self.tex.loading = false;
        match r {
            Ok(c) => {
                if let Err(e) = textures::save_catalog(&self.cache_dir, &c) {
                    self.status = format!("couldn't cache the texture pack list: {e:#}");
                }
                self.status = format!("{} texture packs listed", c.packs.len());
                self.tex.catalog = Some(c);
                self.tex_match();
            }
            Err(e) => self.status = format!("couldn't fetch texture pack lists: {e:#}"),
        }
    }
}
