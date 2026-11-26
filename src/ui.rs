use crate::app::{
    App, CfgRow, LIB_GROUP_LIMIT, LIVE_GROUP, LIVE_SOURCES, LibInput, Menu, Remote, Row, SearchRow, SetRow, SetState, Tab, TexRow,
};
use crate::jobs::LocalState;
use crate::systems;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn border(title: impl Into<String>, focused: bool) -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded).title(title.into()).border_style(if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    })
}

fn sys_span(system: &str) -> Span<'static> {
    if system.is_empty() {
        Span::styled("→ ? (press s)", Style::new().fg(Color::Red).bold())
    } else {
        Span::styled(format!("→ {system}"), Style::new().fg(Color::Magenta))
    }
}

const HL: Style = Style::new().bg(Color::Rgb(40, 60, 90)).add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().fg(Color::DarkGray);

pub fn draw(f: &mut Frame, app: &mut App) {
    let [tabs_area, body, status, help] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(1), Constraint::Length(1)]).areas(f.area());

    let idx = Tab::ALL.iter().position(|t| *t == app.tab).unwrap();
    let active = app.active_downloads();
    f.render_widget(
        Tabs::new(Tab::ALL.iter().map(|t| match t {
            Tab::Library if active > 0 => format!("{} ({} ⇣{active})", t.title(), app.lib.games.len()),
            Tab::Library => format!("{} ({})", t.title(), app.lib.games.len()),
            Tab::Textures if !app.tex.jobs.is_empty() => format!("{} (⇣{})", t.title(), app.tex.jobs.len()),
            Tab::Sources if app.busy() => format!("{} (indexing {})", t.title(), app.sets.len() - app.indexed_sets()),
            Tab::Config if !app.cfg_tab.restart_for.is_empty() => format!("{} (restart)", t.title()),
            Tab::Search | Tab::Textures | Tab::Sources | Tab::Config => t.title().to_string(),
        }))
        .select(idx)
        .highlight_style(Style::new().fg(Color::Cyan).bold().underlined()),
        tabs_area,
    );

    match app.tab {
        Tab::Search => draw_search(f, app, body),
        Tab::Library => draw_library(f, app, body),
        Tab::Textures => draw_textures(f, app, body),
        Tab::Sources => draw_sources(f, app, body),
        Tab::Config => draw_config(f, app, body),
    }

    f.render_widget(Paragraph::new(app.status.as_str()).fg(Color::Yellow), status);
    let help_text = if app.menu.is_some() && !app.pad {
        "↑↓ move · Enter choose · Esc close · ←→ change system"
    } else if app.pad {
        pad_help(app)
    } else if app.editing {
        "typing: Enter submit · Esc/↓ to list · Tab next tab · Ctrl-C quit"
    } else {
        match app.tab {
            Tab::Search => {
                "Enter add / pick a source · a add best · Space actions · ←→ fold · S change system · s or / search · m menu · Tab library · q quit"
            }
            Tab::Library => {
                "Enter download/open · Space actions · m menu · D download all ready · t add Metacritic top 20 · ←→ fold · d direct · p paste · / filter · s search · S system · o sort name/size · x remove · X delete from disk · r rescan · q quit"
            }
            Tab::Sources => "Enter open group / set actions · Space on/off · ←→ fold · m menu · s search · Tab next tab · q quit",
            Tab::Config => "Enter change / switch · Space actions · m menu · s search · Tab next tab · q quit",
            Tab::Textures => {
                "Enter install / turn on·off · Space actions (pick a build) · m menu · r refresh the pack lists · Tab next tab · q quit"
            }
        }
    };
    f.render_widget(Paragraph::new(help_text).style(DIM), help);

    if let Some(m) = &app.menu {
        draw_menu(f, m, body);
    }
}

fn draw_menu(f: &mut Frame, m: &Menu, area: Rect) {
    let inner_w = m.items.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(0).max(m.title.chars().count().min(60)) + 4;
    let w = (inner_w as u16 + 2).min(area.width);
    let h = (m.items.len() as u16 + 2).min(area.height);
    let [_, col, _] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(w), Constraint::Fill(1)]).areas(area);
    let [_, rect, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(h), Constraint::Fill(1)]).areas(col);
    let items: Vec<ListItem> = m.items.iter().map(|(l, _)| ListItem::new(format!(" {l} "))).collect();
    let mut state = ListState::default().with_selected(Some(m.sel));
    f.render_widget(Clear, rect);
    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(format!(" {} ", m.title))
                    .border_style(Style::new().fg(Color::Cyan)),
            )
            .highlight_style(HL)
            .highlight_symbol("▶"),
        rect,
        &mut state,
    );
}

fn pad_help(app: &App) -> &'static str {
    if app.editing {
        return "typing (Steam+X for keyboard) · [A] done · [B] cancel · ↓ to list";
    }
    if app.menu.is_some() {
        return "[A] choose · [B] close · ←→ change system";
    }
    match app.tab {
        Tab::Search => "[A] add / pick a source · [B] actions · [X] search · ←→ fold · [Y] menu · LB/RB tabs · LT/RT page",
        Tab::Library => "[A] download/open · [B] actions · [X] filter · [Y] menu · ←→ fold · LB/RB tabs · LT/RT page",
        Tab::Textures => "[A] open game / install · ←→ fold · [B] actions · [Y] menu · LB/RB tabs · LT/RT page",
        Tab::Sources => "[A] open / actions · [B] actions (on/off) · ←→ fold · [Y] menu · LB/RB tabs · LT/RT page",
        Tab::Config => "[A] change / switch · [B] actions · [Y] menu · LB/RB tabs · LT/RT page",
    }
}

fn input(f: &mut Frame, area: Rect, title: String, value: &str, editing: bool) {
    let shown = if editing { format!("{value}█") } else { value.to_string() };
    f.render_widget(Paragraph::new(shown).block(border(title, editing)), area);
}

fn draw_search(f: &mut Frame, app: &mut App, area: Rect) {
    let [inp, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    let indexing = if app.busy() {
        format!(" · indexing {}/{} sets…", app.indexed_sets(), app.sets.len())
    } else {
        match app.hays_pending() {
            0 => String::new(),
            n => format!(" · preparing {n} sets for search…"),
        }
    };
    input(
        f,
        inp,
        format!(" Find games · {} files in {} sets + live archive.org{indexing} ", app.total_files(), app.indexed_sets()),
        &app.rom_query,
        app.editing,
    );
    let games = app.search_rows.iter().filter(|r| matches!(r, SearchRow::Game(_))).count();
    // Only the rows on screen: a short query has thousands.
    let (shown, mut state) = window(&mut app.rom_state, app.search_rows.len(), list.height.saturating_sub(2) as usize);
    let items: Vec<ListItem> = app.search_rows[shown]
        .iter()
        .map(|r| match r {
            SearchRow::Header { system, count, collapsed } => {
                let label = if system.is_empty() { "Unknown system (pick one with S)" } else { systems::label(system) };
                let sys = if system.is_empty() { String::new() } else { format!("{system} · ") };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} {label}", if *collapsed { "▸" } else { "▾" }), Style::new().fg(Color::Magenta).bold()),
                    Span::styled(format!("  {sys}{count} game{}", if *count == 1 { "" } else { "s" }), DIM),
                ]))
            }
            SearchRow::Game(hits) => {
                let best = &app.rom_hits[hits[0]];
                let base = best.name.rsplit('/').next().unwrap_or(&best.name);
                let any = |f: &dyn Fn(&crate::app::RomHit) -> bool| hits.iter().any(|&i| f(&app.rom_hits[i]));
                let on_disk = any(&|h| {
                    app.present.contains(&h.url)
                        || app.on_disk.contains(&App::disk_key(&h.system, h.name.rsplit('/').next().unwrap_or(&h.name)))
                });
                let (mark, mark_style) = if on_disk {
                    ("● ", Style::new().fg(Color::Green))
                } else if any(&|h| app.lib.contains(&h.url)) {
                    ("✓ ", Style::new().fg(Color::Blue))
                } else {
                    ("  ", Style::new())
                };
                let bolt = if app.instant(best) { Span::styled("⚡", Style::new().fg(Color::Yellow)) } else { Span::raw("  ") };
                let source = if hits.len() > 1 {
                    Span::styled(format!("  {} sources", hits.len()), Style::new().fg(Color::Cyan))
                } else {
                    Span::styled(format!("  {}", best.source.chars().take(48).collect::<String>()), DIM)
                };
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(mark, mark_style),
                    bolt,
                    Span::raw(" "),
                    Span::raw(match &best.title {
                        Some(t) => format!("{t}  [{base}]"),
                        None => crate::names::parse(&best.system, &best.name).display(),
                    }),
                    Span::raw("  "),
                    Span::styled(if best.size > 0 { human(best.size) } else { "size ?".into() }, Style::new().fg(Color::Green)),
                    source,
                ];
                if best.system.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(sys_span(&best.system));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();
    let live = if app.live_loading { " · searching archive.org…" } else { "" };
    let title = format!(" Results ({games}){live} · ⚡ cached on TorBox · ✓ in library · ● on disk · Enter: add / pick a source ");
    f.render_stateful_widget(List::new(items).block(border(title, !app.editing)).highlight_style(HL), list, &mut state);
}

/// The rows of a long list that fit in `height`, scrolled to keep the selection in view
/// (as `List` would), and a state for drawing just those rows.
fn window(state: &mut ListState, len: usize, height: usize) -> (std::ops::Range<usize>, ListState) {
    let height = height.max(1);
    let sel = state.selected().map(|s| s.min(len.saturating_sub(1)));
    let mut offset = state.offset().min(len.saturating_sub(height));
    if let Some(s) = sel {
        if s < offset {
            offset = s;
        } else if s >= offset + height {
            offset = s + 1 - height;
        }
    }
    *state.offset_mut() = offset;
    let end = (offset + height).min(len);
    (offset..end, ListState::default().with_selected(sel.map(|s| s - offset)))
}

fn ago(secs: u64) -> String {
    match secs {
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

fn draw_sources(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.set_rows();
    if app.sets_cursor.is_none() {
        app.sets_cursor = rows.first().map(App::set_row_key);
    }
    let selected = app.sets_cursor.as_ref().and_then(|c| rows.iter().position(|r| App::set_row_key(r) == *c));
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| match r {
            SetRow::Group { name, sets, off, files, collapsed } => {
                let live = *name == LIVE_GROUP;
                let mut spans = vec![
                    Span::styled(format!("{} {name}", if *collapsed { "▸" } else { "▾" }), Style::new().fg(Color::Magenta).bold()),
                    Span::styled(
                        if live {
                            format!("  {sets} sites")
                        } else {
                            format!("  {sets} set{} · {files} files", if *sets == 1 { "" } else { "s" })
                        },
                        DIM,
                    ),
                ];
                if *off > 0 {
                    let text = if off == sets { "  all off".to_string() } else { format!("  {off} off") };
                    spans.push(Span::styled(text, Style::new().fg(Color::DarkGray).bold()));
                }
                ListItem::new(Line::from(spans))
            }
            SetRow::Live(key) => {
                let on = app.cfg.source_on(key);
                let title = LIVE_SOURCES.iter().find(|(k, _)| k == key).map_or(*key, |(_, t)| t);
                let (badge, color) = if on { ("● on", Color::Green) } else { ("○ off", Color::DarkGray) };
                let mut spans = vec![Span::raw("  "), Span::styled(format!("{badge:<11}"), Style::new().fg(color).bold())];
                spans.push(Span::styled(title.to_string(), if on { Style::new() } else { DIM }));
                ListItem::new(Line::from(spans))
            }
            SetRow::Set(item) => {
                let sys = app.sets.iter().find(|s| &s.item == item).map(|s| s.system.clone()).unwrap_or_default();
                let info = app.set_info(item, &sys);
                let (badge, color, detail) = match &info.state {
                    _ if !info.on => ("○ off", Color::DarkGray, String::new()),
                    SetState::Indexed if info.cached => ("⚡ cached", Color::Yellow, String::new()),
                    SetState::Indexed => ("● indexed", Color::Green, String::new()),
                    SetState::Indexing => ("… indexing", Color::Cyan, String::new()),
                    SetState::Waiting => ("○ waiting", Color::DarkGray, String::new()),
                    SetState::Failed(e) => ("✗ failed", Color::Red, e.chars().take(80).collect()),
                    SetState::Locked => ("🔒 login", Color::DarkGray, "restricted: log in to archive.org on the Config tab".into()),
                };
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(format!("{badge:<11}"), Style::new().fg(color).bold()),
                    Span::styled(info.title.clone(), if info.on { Style::new() } else { DIM }),
                    Span::raw("  "),
                    if info.system.is_empty() {
                        Span::styled("→ several systems", Style::new().fg(Color::Magenta))
                    } else {
                        sys_span(&info.system)
                    },
                ];
                if info.files > 0 {
                    spans.push(Span::styled(format!("  {} games", info.files), DIM));
                    spans.push(Span::styled(format!("  {}", human(info.size)), Style::new().fg(Color::Green)));
                }
                if let Some(a) = info.age.filter(|_| matches!(info.state, SetState::Indexed)) {
                    spans.push(Span::styled(format!("  indexed {}", ago(a)), DIM));
                }
                for (service, state, progress, done) in &info.caching {
                    let text = if *done {
                        format!("  ✓ cached on {service}")
                    } else {
                        format!("  ⇣ {service} {state} {:.0}%", progress * 100.0)
                    };
                    spans.push(Span::styled(text, Style::new().fg(if *done { Color::Yellow } else { Color::Cyan })));
                }
                if !detail.is_empty() {
                    spans.push(Span::styled(format!("  {detail}"), Style::new().fg(Color::Red)));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();
    app.sets_state.select(selected);
    let title = format!(" Sources · {} sets · {} games searchable ", app.sets.len(), app.total_files());
    f.render_stateful_widget(List::new(items).block(border(title, true)).highlight_style(HL), area, &mut app.sets_state);
}

/// (badge, color, detail) for a library game.
fn draw_config(f: &mut Frame, app: &mut App, area: Rect) {
    let list_area = match &app.cfg_tab.edit {
        Some(e) => {
            let [inp, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
            let shown = if e.secret() { "•".repeat(e.text.chars().count()) } else { e.text.clone() };
            input(f, inp, format!(" {} · Enter save · Esc cancel ", e.prompt()), &shown, app.editing);
            list
        }
        None => area,
    };
    let rows = app.cfg_rows();
    let label_w = rows
        .iter()
        .filter_map(|r| match r {
            CfgRow::Item(i) => Some(i.label.chars().count()),
            CfgRow::Header(_) => None,
        })
        .max()
        .unwrap_or(0)
        .min(32);
    let mut selected = None;
    let mut n = 0;
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(row, r)| match r {
            CfgRow::Header(h) => ListItem::new(Span::styled(format!("▾ {h}"), Style::new().fg(Color::Magenta).bold())),
            CfgRow::Item(i) => {
                if n == app.cfg_tab.cursor {
                    selected = Some(row);
                }
                n += 1;
                let value_style = match i.ok {
                    Some(true) => Style::new().fg(Color::Green),
                    Some(false) => Style::new().fg(Color::Red),
                    None => Style::new(),
                };
                let note_style = match i.ok {
                    Some(false) if i.note.starts_with('✗') || i.note.starts_with("applies") => Style::new().fg(Color::Red),
                    Some(true) if i.note.starts_with('✓') => Style::new().fg(Color::Green),
                    _ => DIM,
                };
                let label: String = i.label.chars().take(label_w).collect();
                ListItem::new(Line::from(vec![
                    Span::raw(format!("  {label:<label_w$}  ")),
                    Span::styled(i.value.clone(), value_style.bold()),
                    Span::styled(if i.note.is_empty() { String::new() } else { format!("   {}", i.note) }, note_style),
                ]))
            }
        })
        .collect();
    if selected.is_none() && n > 0 {
        app.cfg_tab.cursor = n - 1;
    }
    app.cfg_tab.state.select(selected);
    let file = app.config_file.as_deref().map(crate::app::tilde).unwrap_or_default();
    let title = format!(" Config · saved to {file} ");
    f.render_stateful_widget(List::new(items).block(border(title, !app.editing)).highlight_style(HL), list_area, &mut app.cfg_tab.state);
}

fn game_status(app: &App, id: &str) -> (String, Color, String) {
    if let Some(LocalState::Downloading { file, done, total }) = app.local.get(id) {
        let pct = if *total > 0 { *done as f64 / *total as f64 * 100.0 } else { 0.0 };
        let size = if *total > 0 { format!("{} / {}", human(*done), human(*total)) } else { human(*done) };
        return (format!("⇣ {pct:>3.0}%"), Color::Cyan, format!("{file}  {size}"));
    }
    let local_only = id.starts_with("local:");
    if app.needs_key.contains(id) {
        let packer = app.lib.get(id).and_then(|g| crate::compress::usable(&g.system));
        return match app.local.get(id) {
            Some(LocalState::Failed(m)) if packer.is_some() => ("✗ failed".into(), Color::Red, m.clone()),
            Some(LocalState::Failed(m)) => ("✗ no key".into(), Color::Red, m.clone()),
            _ if let Some(t) = packer => {
                ("◐ pack".into(), Color::Yellow, format!("Enter packs it as {}: smaller, and it plays without unzipping", t.format()))
            }
            _ if app.lib.get(id).is_some_and(|g| g.system == "xbox360") => {
                ("◐ zipped".into(), Color::Yellow, "Xenia needs the ISO · Enter unzips it".into())
            }
            _ if app.lib.get(id).is_some_and(|g| g.system == "xbox") => {
                ("◐ convert".into(), Color::Yellow, "xemu needs the game partition ISO · Enter unzips/converts it".into())
            }
            _ if app.lib.get(id).is_some_and(|g| g.system != "ps3") => {
                ("◐ zipped".into(), Color::Yellow, "the emulator can't run this disc from a zip · Enter unzips it".into())
            }
            _ => ("◐ needs key".into(), Color::Yellow, "PS3 disc key missing · Enter fetches it".into()),
        };
    }
    if app.present.contains(id) {
        let detail = if local_only { "found in roms folder" } else { "" };
        return ("● on disk".into(), Color::Green, detail.into());
    }
    if local_only {
        return ("✗ missing".into(), Color::Red, "no longer in the roms folder (r to rescan)".into());
    }
    if let Some(LocalState::Failed(m)) = app.local.get(id) {
        return ("✗ failed".into(), Color::Red, m.clone());
    }
    if app.lib.get(id).is_some_and(|g| g.kind == crate::library::SourceKind::Vimm) {
        let dir = app.downloads_dir.display();
        return if app.vimm_waiting.contains_key(id) {
            ("🌐 browser".into(), Color::Yellow, format!("press Download on Vimm's page · watching {dir}"))
        } else {
            ("○ vimm".into(), Color::DarkGray, format!("Enter opens Vimm's page in a browser · picked up from {dir}"))
        };
    }
    if let Some(pos) = app.download_queue.iter().position(|q| q == id) {
        return ("⏳ queued".into(), Color::Cyan, format!("#{} in the download queue", pos + 1));
    }
    match app.remote.get(id) {
        Some(Remote::Ready) => {
            let host = if app.lib.get(id).is_some_and(|g| g.rd_id.is_some()) { "Real-Debrid" } else { "TorBox" };
            ("☁ ready".into(), Color::Blue, format!("on {host} · Enter to download"))
        }
        Some(Remote::Pending { state, progress }) => (format!("☁ {:>3.0}%", progress * 100.0), Color::Yellow, state.clone()),
        Some(Remote::Sending) => ("… sending".into(), Color::Yellow, String::new()),
        Some(Remote::Missing) => ("! gone".into(), Color::Red, "no longer on TorBox · Enter re-sends".into()),
        Some(Remote::Error(m)) => ("✗ torbox".into(), Color::Red, m.clone()),
        Some(Remote::NoKey) => ("○ local".into(), Color::DarkGray, "no TorBox key · Enter downloads directly".into()),
        Some(Remote::Direct) => {
            if app.lib.get(id).is_some_and(|g| crate::ifarchive::is_url(&g.source)) {
                ("○ free".into(), Color::DarkGray, "IF Archive · Enter downloads it".into())
            } else {
                (
                    "○ direct".into(),
                    Color::DarkGray,
                    "restricted on archive.org (TorBox can't fetch it) · Enter downloads it with your login".into(),
                )
            }
        }
        Some(Remote::NotSent) | None => ("○ not sent".into(), Color::DarkGray, "Enter sends to TorBox".into()),
    }
}

fn draw_library(f: &mut Frame, app: &mut App, area: Rect) {
    let [inp, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    let pasting = app.lib_input == LibInput::Paste;
    if pasting {
        let target = match systems::guess_from_link(&app.lib_paste) {
            Some(s) => format!("→ {s} ({})", systems::label(s)),
            None => "→ system detected from files".into(),
        };
        input(f, inp, format!(" Paste magnet / URL {target} · Enter add · Esc cancel "), &app.lib_paste, app.editing);
    } else {
        let sort = match app.lib_sort {
            crate::app::LibSort::Name => "by name",
            crate::app::LibSort::Size => "by size, biggest first",
        };
        input(f, inp, format!(" Filter library (p to paste a magnet/URL) · sorted {sort} (o) "), &app.lib_filter, app.editing);
    }

    let rows = app.lib_rows();
    let selected_row = app.lib_cursor.as_ref().and_then(|c| rows.iter().position(|r| app.row_key(r) == *c));
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| match r {
            Row::Header { system, count, collapsed } => {
                let label = if system.is_empty() {
                    "Unsorted (system detected on download)".to_string()
                } else {
                    systems::label(system).to_string()
                };
                let in_sys = || app.lib.games.iter().filter(|g| &g.system == system);
                let on_disk = in_sys().filter(|g| app.present.contains(&g.id)).count();
                let ready =
                    in_sys().filter(|g| !app.present.contains(&g.id) && matches!(app.remote.get(&g.id), Some(Remote::Ready))).count();
                let mut spans = vec![
                    Span::styled(format!("{} {label}", if *collapsed { "▸" } else { "▾" }), Style::new().fg(Color::Magenta).bold()),
                    Span::styled(
                        format!(
                            "  {}{count} games · {on_disk} on disk",
                            if system.is_empty() { String::new() } else { format!("{system} · ") }
                        ),
                        DIM,
                    ),
                ];
                if ready > 0 {
                    spans.push(Span::styled(format!(" · {ready} ready"), Style::new().fg(Color::Blue)));
                }
                let total: u64 = in_sys().map(|g| app.size_on_disk(g)).sum();
                if total > 0 {
                    spans.push(Span::styled(format!(" · {} on disk", human(total)), Style::new().fg(Color::Green)));
                }
                ListItem::new(Line::from(spans))
            }
            Row::More { system, hidden } => ListItem::new(Line::from(vec![
                Span::styled(format!("    … {hidden} more"), Style::new().fg(Color::Yellow)),
                Span::styled(format!("  (showing {LIB_GROUP_LIMIT}: Enter lists every {system} game, or find one with / or Search)"), DIM),
            ])),
            Row::Game(i) => {
                let g = &app.lib.games[*i];
                let (badge, color, detail) = game_status(app, &g.id);
                let mut spans =
                    vec![Span::raw("  "), Span::styled(format!("{badge:<11}"), Style::new().fg(color).bold()), Span::raw(app.game_name(g))];
                // on disk: what it really takes; not yet: the source's size, dimmed
                match app.size_on_disk(g) {
                    0 if g.size > 0 => spans.push(Span::styled(format!("  {}", human(g.size)), DIM)),
                    0 => {}
                    n => spans.push(Span::styled(format!("  {}", human(n)), Style::new().fg(Color::Green))),
                }
                if !detail.is_empty() {
                    spans.push(Span::styled(format!("  {detail}"), DIM));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();
    app.lib_state.select(selected_row);

    let on_disk = app.present.len();
    let ready =
        app.lib.games.iter().filter(|g| matches!(app.remote.get(&g.id), Some(Remote::Ready)) && !app.present.contains(&g.id)).count();
    let title =
        format!(" Library · {} games · {on_disk} on disk · {ready} ready on TorBox · {} ", app.lib.games.len(), app.cfg.roms_dir.display());
    let empty = app.lib.games.is_empty();
    let list_widget = List::new(items).block(border(title, !app.editing)).highlight_style(HL);
    f.render_stateful_widget(list_widget, list, &mut app.lib_state);
    if empty {
        let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)]).areas(list);
        f.render_widget(
            Paragraph::new("Nothing here yet. Find games in tab 1 and press Enter, or press p to paste a link.").style(DIM).centered(),
            mid,
        );
    }
}

fn tex_status(app: &App, key: &str) -> (String, Color, String) {
    use crate::textures::TexState;
    match app.tex.jobs.get(key) {
        Some(TexState::Remote { what, progress }) if *progress > 0.0 => {
            ("☁ debrid".into(), Color::Blue, format!("{what} {:.0}%", progress * 100.0))
        }
        Some(TexState::Remote { what, .. }) => ("☁ debrid".into(), Color::Blue, what.clone()),
        Some(TexState::Downloading { done, total }) => {
            let pct = if *total > 0 { *done as f64 * 100.0 / *total as f64 } else { 0.0 };
            (format!("⇣ {pct:>3.0}%"), Color::Cyan, if *total > 0 { format!("{} / {}", human(*done), human(*total)) } else { human(*done) })
        }
        Some(TexState::Installing) => ("⚙ unpacking".into(), Color::Cyan, String::new()),
        Some(TexState::Failed(m)) => ("✗ failed".into(), Color::Red, m.clone()),
        Some(TexState::Done(_)) | None => match app.tex.installed.get(key) {
            Some(i) if i.enabled => ("● on".into(), Color::Green, human(i.size)),
            Some(i) => ("○ off".into(), Color::Yellow, human(i.size)),
            None => (String::new(), Color::Reset, String::new()),
        },
    }
}

/// The selected pack's write-up: status, region, where it's listed and its overview.
fn draw_pack_about(f: &mut Frame, p: &crate::textures::Pack, area: Rect) {
    let mut facts = Vec::new();
    if let Some(s) = &p.status {
        let color = if s.eq_ignore_ascii_case("complete") { Color::Green } else { Color::Yellow };
        facts.push(Span::styled(s.clone(), Style::new().fg(color).bold()));
    }
    if let Some(r) = &p.region {
        facts.push(Span::raw(format!("region {r}")));
    }
    if let Some(page) = &p.page {
        facts.push(Span::styled(page.clone(), DIM));
    }
    let mut first = Vec::new();
    for (i, s) in facts.into_iter().enumerate() {
        if i > 0 {
            first.push(Span::styled(" · ", DIM));
        }
        first.push(s);
    }
    let mut lines = vec![Line::from(first)];
    // the list's one-line description stands in when the page had no overview
    if p.about.is_empty() && !p.summary.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(p.summary.clone(), Style::new().fg(Color::Gray).italic()));
    }
    for para in &p.about {
        lines.push(Line::raw(""));
        lines.push(Line::raw(para.clone()));
    }
    let who = if p.author.is_empty() { p.name.clone() } else { format!("{} · {}", p.game, p.author) };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(border(format!(" {who} "), false)), area);
}

fn draw_textures(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.tex_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| match r {
            TexRow::System { system, with_packs, on_disk, packs, collapsed } => {
                let arrow = if *collapsed { "▸" } else { "▾" };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{arrow} {}", systems::label(system)), Style::new().fg(Color::Magenta).bold()),
                    Span::styled(
                        format!("  {with_packs} of {on_disk} on disk · {packs} pack{}", if *packs == 1 { "" } else { "s" }),
                        DIM,
                    ),
                ]))
            }
            TexRow::Game { game, packs, enabled } => {
                let g = &app.lib.games[*game];
                let expanded = app.tex.expanded.contains(&g.id);
                let arrow = if *packs == 0 {
                    " "
                } else if expanded {
                    "▾"
                } else {
                    "▸"
                };
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(format!("{arrow} "), Style::new().fg(Color::Cyan)),
                    Span::styled(app.game_name(g), Style::new().bold()),
                ];
                if *enabled > 0 {
                    spans.push(Span::styled(format!("  ● {enabled} on"), Style::new().fg(Color::Green).bold()));
                }
                spans.push(Span::styled(format!("  {packs} pack{}", if *packs == 1 { "" } else { "s" }), DIM));
                ListItem::new(Line::from(spans))
            }
            TexRow::Pack(_, p) => {
                let Some(p) = app.tex.pack(*p) else { return ListItem::new("") };
                let (badge, color, detail) = tex_status(app, &p.key);
                let builds: Vec<String> =
                    p.builds.iter().map(|b| if b.size > 0 { format!("{} {}", b.label, human(b.size)) } else { b.label.clone() }).collect();
                let only_web = p.downloadable().next().is_none();
                let mut spans = vec![
                    Span::raw("    "),
                    Span::styled(format!("{badge:<11}"), Style::new().fg(color).bold()),
                    Span::raw(if p.author.is_empty() { p.name.clone() } else { p.author.clone() }),
                    Span::styled(format!("  {}", builds.join(" · ")), Style::new().fg(Color::Green)),
                    Span::styled(format!("  {}", p.sources.join(" + ")), DIM),
                ];
                if only_web && badge.is_empty() {
                    spans.push(Span::styled("  🌐 web page only", Style::new().fg(Color::Yellow)));
                }
                if !detail.is_empty() {
                    spans.push(Span::styled(format!("  {detail}"), DIM));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();
    let sel = app.tex_selected();
    app.tex.state.select(sel);
    let picked = sel.and_then(|i| match rows.get(i) {
        Some(TexRow::Pack(_, p)) => app.tex.pack(*p).cloned(),
        _ => None,
    });
    let (area, about) = match &picked {
        Some(p) if !p.about.is_empty() || !p.summary.is_empty() || p.status.is_some() || p.region.is_some() => {
            let [list, about] = Layout::vertical([Constraint::Min(6), Constraint::Length(9)]).areas(area);
            (list, Some(about))
        }
        _ => (area, None),
    };
    let packs = app.tex.catalog.as_ref().map_or(0, |c| c.packs.len());
    let loading = if app.tex.loading { " · fetching lists…" } else { "" };
    let title = format!(" Texture packs · {packs} listed{loading} · {} ", app.tex.dirs.root.display());
    let empty = rows.iter().all(|r| matches!(r, TexRow::System { with_packs: 0, .. }));
    f.render_stateful_widget(List::new(items).block(border(title, true)).highlight_style(HL), area, &mut app.tex.state);
    if let (Some(p), Some(rect)) = (&picked, about) {
        draw_pack_about(f, p, rect);
    }
    if empty {
        let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let msg = if app.tex.loading {
            "Fetching the texture pack lists…"
        } else {
            "No texture packs for the games on disk (yet). r refreshes the lists."
        };
        f.render_widget(Paragraph::new(msg).style(DIM).centered(), mid);
    }
}
