//! The Config tab: API keys, the archive.org login, folders and other settings, saved to
//! config.toml. What can change while running does; the rest asks for a restart.

use super::{Act, App, Menu, Msg, SetRef};
use crate::config::Collection;
use crate::gamepad;
use ratatui::widgets::ListState;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A setting (a row of the Config tab).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Restart,
    TorBoxKey,
    RdKey,
    Archive,
    RomsDir,
    DownloadsDir,
    Browser,
    Poll,
    /// `extra_sets[i]`.
    ExtraSet(usize),
    AddSet,
}

/// What the Config tab's row menu does to the row's setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfgAct {
    /// Type a new value.
    Edit,
    /// Back to the default / remove the key / log out / drop the set.
    Clear,
    /// Ask the service whether the key works.
    Check,
    Restart,
}

pub enum CfgRow {
    Header(&'static str),
    Item(CfgItem),
}

pub struct CfgItem {
    pub field: Field,
    pub label: String,
    pub value: String,
    pub note: String,
    /// Shown green (Some(true)) or red (Some(false)).
    pub ok: Option<bool>,
}

/// What a key check came back with.
pub enum Check {
    Running,
    Ok(String),
    Failed(String),
}

/// A setting being typed.
pub struct CfgEdit {
    pub field: Field,
    /// archive.org login: the email, once typed (the password is being typed).
    pub email: Option<String>,
    pub text: String,
}

impl CfgEdit {
    /// Typed text isn't shown (keys, passwords).
    pub fn secret(&self) -> bool {
        matches!(self.field, Field::TorBoxKey | Field::RdKey) || self.email.is_some()
    }

    pub fn prompt(&self) -> String {
        match (self.field, &self.email) {
            (Field::TorBoxKey, _) => "TorBox API key (torbox.app/settings)".into(),
            (Field::RdKey, _) => "Real-Debrid API key (real-debrid.com/apitoken)".into(),
            (Field::Archive, None) => "archive.org email".into(),
            (Field::Archive, Some(e)) => format!("archive.org password for {e} (not kept)"),
            (Field::RomsDir, _) => "RetroDECK roms folder (holds snes/, psx/, …)".into(),
            (Field::DownloadsDir, _) => "Browser downloads folder (empty: ~/Downloads)".into(),
            (Field::Browser, _) => {
                "Browser command for Vimm pages, the URL is added (empty: Firefox/Chrome; e.g. terminal-browser open)".into()
            }
            (Field::Poll, _) => "Seconds between TorBox checks while games download".into(),
            (Field::AddSet, _) => "archive.org identifier or URL of a set to search".into(),
            _ => String::new(),
        }
    }
}

#[derive(Default)]
pub struct CfgTab {
    /// Index of the selected item (headers don't count).
    pub cursor: usize,
    pub state: ListState,
    pub edit: Option<CfgEdit>,
    /// Settings changed that only apply after a restart.
    pub restart_for: Vec<&'static str>,
    pub checks: HashMap<Field, Check>,
    pub logging_in: bool,
    opened: bool,
}

fn mask(key: &str) -> String {
    let n = key.chars().count();
    let tail: String = if n > 8 { key.chars().skip(n - 4).collect() } else { String::new() };
    format!("••••••••{tail}")
}

fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

/// A path with the home folder shown as `~`.
pub fn tilde(p: &Path) -> String {
    match home().and_then(|h| p.strip_prefix(&h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// A typed path: `~` expands to the home folder.
fn expand(s: &str) -> PathBuf {
    match (s.strip_prefix('~'), home()) {
        (Some(rest), Some(h)) if rest.is_empty() || rest.starts_with('/') => h.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(s),
    }
}

/// "https://archive.org/details/foo/bar.zip" → "foo".
fn archive_identifier(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches('/');
    let id = match s.find("archive.org/") {
        Some(i) => {
            let rest = &s[i + "archive.org/".len()..];
            let rest = rest.strip_prefix("details/").or_else(|| rest.strip_prefix("download/"))?;
            rest.split(['/', '?', '#']).next()?
        }
        None => s,
    };
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))).then(|| id.to_string())
}

fn env_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| !v.trim().is_empty())
}

impl App {
    /// The Config tab's rows.
    pub fn cfg_rows(&self) -> Vec<CfgRow> {
        let mut rows = Vec::new();
        let item =
            |field, label: &str, value: String, note: String, ok| CfgRow::Item(CfgItem { field, label: label.into(), value, note, ok });
        if !self.cfg_tab.restart_for.is_empty() {
            rows.push(CfgRow::Header("Restart needed"));
            rows.push(item(
                Field::Restart,
                "Restart romgetter",
                format!("applies {}", self.cfg_tab.restart_for.join(", ")),
                String::new(),
                Some(false),
            ));
        }
        let check = |f: Field| match self.cfg_tab.checks.get(&f) {
            Some(Check::Running) => ("checking…".to_string(), None),
            Some(Check::Ok(who)) => (format!("✓ {who}"), Some(true)),
            Some(Check::Failed(why)) => (format!("✗ {why}"), Some(false)),
            None => (String::new(), None),
        };
        rows.push(CfgRow::Header("Accounts"));
        for (field, label, key, var, hint) in [
            (Field::TorBoxKey, "TorBox API key", self.cfg.api_key(), "TORBOX_API_KEY", "games come through TorBox"),
            (Field::RdKey, "Real-Debrid API key", self.cfg.rd_key(), "REAL_DEBRID_API_KEY", "for Minerva games TorBox can't get"),
        ] {
            let (mut note, ok) = check(field);
            if env_set(var) {
                note = format!("{var} in the environment wins · {note}");
            } else if key.is_none() {
                note = hint.into();
            }
            rows.push(item(field, label, key.as_deref().map_or_else(|| "not set".into(), mask), note, ok));
        }
        let (value, note) = match (&self.cfg.archive_login, self.cfg_tab.logging_in) {
            (_, true) => ("logging in…".into(), String::new()),
            (Some(l), _) => (format!("logged in as {}", l.user), "restricted sets show up and download directly".into()),
            (None, _) => ("not logged in".into(), "log in to search restricted sets".into()),
        };
        rows.push(item(Field::Archive, "archive.org", value, note, self.cfg.archive_login.as_ref().map(|_| true)));

        rows.push(CfgRow::Header("Folders"));
        let missing = !self.cfg.roms_dir.is_dir();
        rows.push(item(
            Field::RomsDir,
            "ROMs folder",
            tilde(&self.cfg.roms_dir),
            if missing { "✗ not there".into() } else { "RetroDECK's roms folder".into() },
            missing.then_some(false),
        ));
        rows.push(item(
            Field::DownloadsDir,
            "Browser downloads",
            tilde(&self.downloads_dir) + if self.cfg.downloads_dir.is_none() { " (default)" } else { "" },
            "watched for Vimm games".into(),
            None,
        ));

        rows.push(CfgRow::Header("Other"));
        rows.push(item(
            Field::Browser,
            "Browser for Vimm",
            self.cfg.browser.clone().unwrap_or_else(|| "default (Firefox or Chrome)".into()),
            String::new(),
            None,
        ));
        rows.push(item(Field::Poll, "TorBox check every", format!("{} s", self.cfg.poll_secs), "while games download".into(), None));

        rows.push(CfgRow::Header("archive.org · your extra sets"));
        for (i, c) in self.cfg.extra_sets.iter().enumerate() {
            let state = match self.index.get(&c.item) {
                Some(idx) => format!("{} files", idx.files.len()),
                None if self.indexing.contains(&c.item) => "indexing…".into(),
                None => self.index_errors.get(&c.item).cloned().unwrap_or_default(),
            };
            let note = [if c.system.is_empty() { String::new() } else { format!("→ {}", c.system) }, state]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            rows.push(item(Field::ExtraSet(i), &c.item, c.title.clone(), note, None));
        }
        rows.push(item(Field::AddSet, "+ Add a set…", String::new(), String::new(), None));
        rows
    }

    fn cfg_items(&self) -> Vec<Field> {
        self.cfg_rows()
            .into_iter()
            .filter_map(|r| match r {
                CfgRow::Item(i) => Some(i.field),
                CfgRow::Header(_) => None,
            })
            .collect()
    }

    pub fn cfg_field(&self) -> Option<Field> {
        let items = self.cfg_items();
        items.get(self.cfg_tab.cursor.min(items.len().saturating_sub(1))).copied()
    }

    pub(super) fn cfg_move(&mut self, delta: isize) {
        let n = self.cfg_items().len() as isize;
        self.cfg_tab.cursor = (self.cfg_tab.cursor as isize + delta).clamp(0, (n - 1).max(0)) as usize;
    }

    fn cfg_select(&mut self, f: Field) {
        if let Some(i) = self.cfg_items().iter().position(|x| *x == f) {
            self.cfg_tab.cursor = i;
        }
    }

    /// First visit: check the keys.
    pub(super) fn cfg_open(&mut self) {
        if !std::mem::replace(&mut self.cfg_tab.opened, true) {
            self.cfg_check(Field::TorBoxKey);
            self.cfg_check(Field::RdKey);
        }
    }

    /// Ask TorBox / Real-Debrid who the configured key belongs to.
    fn cfg_check(&mut self, field: Field) {
        let (http, tx) = (self.ctx.http.clone(), self.tx.clone());
        match field {
            Field::TorBoxKey => {
                let Some(key) = self.cfg.api_key() else { return };
                let tb = crate::torbox::TorBox::new(http, key);
                tokio::spawn(async move {
                    let _ = tx.send(Msg::KeyChecked(field, tb.whoami().await));
                });
            }
            Field::RdKey => {
                let Some(key) = self.cfg.rd_key() else { return };
                let rd = crate::realdebrid::RealDebrid::new(http, key);
                tokio::spawn(async move {
                    let _ = tx.send(Msg::KeyChecked(field, rd.whoami().await));
                });
            }
            _ => return,
        }
        self.cfg_tab.checks.insert(field, Check::Running);
    }

    pub(super) fn on_key_checked(&mut self, field: Field, res: anyhow::Result<String>) {
        let name = if field == Field::TorBoxKey { "TorBox" } else { "Real-Debrid" };
        let check = match res {
            Ok(who) => Check::Ok(who),
            Err(e) => {
                self.status = format!("{name} didn't take the key: {e:#}");
                Check::Failed(format!("{e:#}").chars().take(60).collect())
            }
        };
        self.cfg_tab.checks.insert(field, check);
    }

    pub(super) fn on_archive_login(&mut self, res: anyhow::Result<crate::config::ArchiveLogin>) {
        self.cfg_tab.logging_in = false;
        match res {
            Ok(login) => {
                self.status = format!("logged in to archive.org as {}", login.user);
                self.cfg.archive_login = Some(login);
                self.save_config();
                self.needs_restart("the archive.org login");
            }
            Err(e) => self.status = format!("archive.org login failed: {e:#}"),
        }
    }

    fn needs_restart(&mut self, what: &'static str) {
        if self.cfg_tab.restart_for.is_empty() {
            self.cfg_tab.cursor += 1; // the restart row goes in above: stay on the same setting
        }
        if !self.cfg_tab.restart_for.contains(&what) {
            self.cfg_tab.restart_for.push(what);
        }
        if !self.status.contains("restart") {
            self.status.push_str(" · restart romgetter to apply (Config tab)");
        }
    }

    fn cfg_start_edit(&mut self, field: Field) {
        let text = match field {
            Field::RomsDir => tilde(&self.cfg.roms_dir),
            Field::DownloadsDir => self.cfg.downloads_dir.as_deref().map(tilde).unwrap_or_default(),
            Field::Browser => self.cfg.browser.clone().unwrap_or_default(),
            Field::Poll => self.cfg.poll_secs.to_string(),
            _ => String::new(),
        };
        self.cfg_tab.edit = Some(CfgEdit { field, email: None, text });
        self.editing = true;
        if self.pad {
            gamepad::open_steam_keyboard();
        }
    }

    /// Enter / A on a row: its main action.
    pub(super) fn cfg_activate(&mut self) {
        let Some(f) = self.cfg_field() else { return };
        match f {
            Field::Restart => self.cfg_run(CfgAct::Restart),
            Field::Archive if self.cfg.archive_login.is_some() => self.cfg_item_menu(),
            Field::ExtraSet(_) => self.cfg_item_menu(),
            _ => self.cfg_run(CfgAct::Edit),
        }
    }

    /// Space / B on a row: what can be done with it.
    pub(super) fn cfg_item_menu(&mut self) {
        let Some(f) = self.cfg_field() else { return };
        let mut items: Vec<(String, Act)> = Vec::new();
        let edit = |s: &str| (s.to_string(), Act::Cfg(CfgAct::Edit));
        let clear = |s: &str| (s.to_string(), Act::Cfg(CfgAct::Clear));
        let title = match f {
            Field::Restart => {
                items.push(("Restart romgetter".into(), Act::Cfg(CfgAct::Restart)));
                "Restart".to_string()
            }
            Field::TorBoxKey | Field::RdKey => {
                let set = if f == Field::TorBoxKey { self.cfg.torbox_api_key.is_some() } else { self.cfg.real_debrid_api_key.is_some() };
                items.push(edit(if set { "Change the key…" } else { "Enter a key…" }));
                if set {
                    items.push(clear("Remove the key"));
                }
                if (f == Field::TorBoxKey && self.cfg.api_key().is_some()) || (f == Field::RdKey && self.cfg.rd_key().is_some()) {
                    items.push(("Check the key".into(), Act::Cfg(CfgAct::Check)));
                }
                if f == Field::TorBoxKey { "TorBox" } else { "Real-Debrid" }.to_string()
            }
            Field::Archive => {
                items.push(edit(if self.cfg.archive_login.is_some() { "Log in as someone else…" } else { "Log in…" }));
                if self.cfg.archive_login.is_some() {
                    items.push(clear("Log out"));
                }
                "archive.org".to_string()
            }
            Field::RomsDir | Field::DownloadsDir | Field::Browser | Field::Poll => {
                items.push(edit("Change…"));
                items.push(clear("Back to the default"));
                match f {
                    Field::RomsDir => "ROMs folder",
                    Field::DownloadsDir => "Browser downloads",
                    Field::Browser => "Browser for Vimm",
                    _ => "TorBox check every",
                }
                .to_string()
            }
            Field::ExtraSet(i) => {
                let Some(c) = self.cfg.extra_sets.get(i) else { return };
                items.push(clear("Remove this set"));
                c.item.clone()
            }
            Field::AddSet => {
                items.push(edit("Add an archive.org set…"));
                "Extra sets".to_string()
            }
        };
        items.push(("Close".into(), Act::Close));
        self.menu = Some(Menu { title, items, sel: 0 });
    }

    pub(super) fn cfg_run(&mut self, act: CfgAct) {
        let Some(f) = self.cfg_field() else { return };
        match act {
            CfgAct::Restart => {
                self.restart = true;
                self.quit = true;
            }
            CfgAct::Check => self.cfg_check(f),
            CfgAct::Edit => match f {
                Field::Restart | Field::ExtraSet(_) => {}
                _ => self.cfg_start_edit(f),
            },
            CfgAct::Clear => {
                match f {
                    Field::TorBoxKey => {
                        self.cfg.torbox_api_key = None;
                        self.cfg_tab.checks.remove(&f);
                        self.status = "TorBox key removed".into();
                        self.needs_restart("the TorBox key");
                    }
                    Field::RdKey => {
                        self.cfg.real_debrid_api_key = None;
                        self.cfg_tab.checks.remove(&f);
                        self.status = "Real-Debrid key removed".into();
                        self.needs_restart("the Real-Debrid key");
                    }
                    Field::Archive => {
                        self.cfg.archive_login = None;
                        self.status = "logged out of archive.org".into();
                        self.needs_restart("the archive.org login");
                    }
                    Field::RomsDir => self.set_roms_dir(crate::config::Config::default().roms_dir),
                    Field::DownloadsDir => {
                        self.cfg.downloads_dir = None;
                        self.downloads_dir = crate::vimm::default_downloads_dir();
                        self.status = format!("watching {} for Vimm games", tilde(&self.downloads_dir));
                    }
                    Field::Browser => {
                        self.cfg.browser = None;
                        self.status = "Vimm pages open in Firefox or Chrome".into();
                    }
                    Field::Poll => self.set_poll(crate::config::Config::default().poll_secs),
                    Field::ExtraSet(i) => self.remove_extra_set(i),
                    _ => return,
                }
                self.save_config();
            }
        }
    }

    fn set_roms_dir(&mut self, dir: PathBuf) {
        if dir == self.cfg.roms_dir {
            self.status = "ROMs folder unchanged".into();
            return;
        }
        self.status = format!("ROMs folder: {}{}", tilde(&dir), if dir.is_dir() { "" } else { " (not there yet)" });
        self.cfg.roms_dir = dir;
        self.needs_restart("the ROMs folder");
    }

    fn set_poll(&mut self, secs: u64) {
        self.cfg.poll_secs = secs;
        self.ctx.poll = Duration::from_secs(secs.max(2));
        self.status = format!("checking TorBox every {secs} s while games download");
    }

    fn remove_extra_set(&mut self, i: usize) {
        if i >= self.cfg.extra_sets.len() {
            return;
        }
        let c = self.cfg.extra_sets.remove(i);
        let builtin = self.cfg.use_default_sets && crate::sources::default_sets().iter().any(|(item, _)| *item == c.item);
        if !builtin {
            self.sets.retain(|s| s.item != c.item);
            self.index.remove(&c.item);
            self.hay.remove(&c.item);
            self.on_query_change();
        }
        self.status = format!("removed {} from the extra sets", c.item);
        self.cfg_move(0);
    }

    fn add_extra_set(&mut self, text: &str) {
        let Some(item) = archive_identifier(text) else {
            self.status = format!("'{}' isn't an archive.org identifier or URL", text.trim());
            return;
        };
        if self.sets.iter().any(|s| s.item == item) || self.cfg.extra_sets.iter().any(|c| c.item == item) {
            self.status = format!("{item} is already searched");
            return;
        }
        self.cfg.extra_sets.push(Collection { item: item.clone(), title: String::new(), system: String::new() });
        self.sets.push(SetRef { item: item.clone(), system: String::new() });
        self.start_index(item.clone());
        self.save_config();
        self.status = format!("added {item}; indexing it for Search");
        self.cfg_select(Field::ExtraSet(self.cfg.extra_sets.len() - 1));
    }

    /// Enter while typing a setting.
    pub(super) fn cfg_submit(&mut self) {
        let Some(edit) = self.cfg_tab.edit.take() else { return };
        let text = edit.text.trim().to_string();
        match edit.field {
            Field::TorBoxKey | Field::RdKey if text.is_empty() => self.status = "key unchanged".into(),
            Field::TorBoxKey => {
                self.cfg.torbox_api_key = Some(text);
                self.save_config();
                self.status = "TorBox key saved".into();
                self.needs_restart("the TorBox key");
                self.cfg_check(Field::TorBoxKey);
            }
            Field::RdKey => {
                self.cfg.real_debrid_api_key = Some(text);
                self.save_config();
                self.status = "Real-Debrid key saved".into();
                self.needs_restart("the Real-Debrid key");
                self.cfg_check(Field::RdKey);
            }
            Field::Archive => match edit.email {
                None if text.is_empty() => {}
                None => {
                    self.cfg_tab.edit = Some(CfgEdit { field: Field::Archive, email: Some(text), text: String::new() });
                    self.editing = true;
                }
                Some(email) => {
                    let password = edit.text; // as typed: spaces count
                    self.cfg_tab.logging_in = true;
                    self.status = format!("logging in to archive.org as {email}…");
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(Msg::ArchiveLogin(crate::archive::login(&email, &password).await));
                    });
                }
            },
            Field::RomsDir => {
                let dir = if text.is_empty() { crate::config::Config::default().roms_dir } else { expand(&text) };
                self.set_roms_dir(dir);
                self.save_config();
            }
            Field::DownloadsDir => {
                self.cfg.downloads_dir = (!text.is_empty()).then(|| expand(&text));
                self.downloads_dir = self.cfg.downloads_dir.clone().unwrap_or_else(crate::vimm::default_downloads_dir);
                self.save_config();
                self.status = format!("watching {} for Vimm games", tilde(&self.downloads_dir));
            }
            Field::Browser => {
                self.cfg.browser = (!text.is_empty()).then_some(text);
                self.save_config();
                self.status = match &self.cfg.browser {
                    Some(b) => format!("Vimm pages open with: {b} <url>"),
                    None => "Vimm pages open in Firefox or Chrome".into(),
                };
            }
            Field::Poll => match text.parse::<u64>() {
                Ok(n) if n >= 2 => {
                    self.set_poll(n);
                    self.save_config();
                }
                _ => self.status = format!("'{text}': give a number of seconds, 2 or more"),
            },
            Field::AddSet if !text.is_empty() => self.add_extra_set(&text),
            _ => {}
        }
    }

    /// Esc while typing a setting.
    pub(super) fn cfg_cancel(&mut self) {
        if self.cfg_tab.edit.take().is_some() {
            self.status = "unchanged".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_archive_identifiers() {
        assert_eq!(archive_identifier("https://archive.org/details/foo-bar_1/x.zip").as_deref(), Some("foo-bar_1"));
        assert_eq!(archive_identifier("archive.org/download/foo/").as_deref(), Some("foo"));
        assert_eq!(archive_identifier(" foo.bar ").as_deref(), Some("foo.bar"));
        assert_eq!(archive_identifier("https://archive.org/search?q=x"), None);
        assert_eq!(archive_identifier("two words"), None);
    }

    #[test]
    fn masks_and_paths() {
        assert_eq!(mask("abcdefghijkl"), "••••••••ijkl");
        assert_eq!(mask("short"), "••••••••");
        if let Some(h) = home() {
            assert_eq!(expand("~/roms"), h.join("roms"));
            assert_eq!(tilde(&h.join("roms")), "~/roms");
            assert_eq!(expand("~"), h);
        }
        assert_eq!(expand("/x/~y"), PathBuf::from("/x/~y"));
    }
}
