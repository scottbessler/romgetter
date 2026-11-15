use crate::app::{App, LIB_GROUP_LIMIT, LibInput, Menu, Remote, Row, Tab, TexRow};
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
            Tab::Search | Tab::Textures => t.title().to_string(),
        }))
        .select(idx)
        .highlight_style(Style::new().fg(Color::Cyan).bold().underlined()),
        tabs_area,
    );

    match app.tab {
        Tab::Search => draw_search(f, app, body),
        Tab::Library => draw_library(f, app, body),
        Tab::Textures => draw_textures(f, app, body),
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
            Tab::Search => "Enter/a add to library · Space actions · s/S change system · / search · m menu · Tab library · q quit",
            Tab::Library => {
                "Enter download/open · Space actions · m menu · D download all ready · t add Metacritic top 20 · ←→ fold · d direct · p paste · / filter · s system · o sort name/size · x remove · X delete from disk · r rescan · q quit"
            }
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
        Tab::Search => "[A] add to library · [B] actions · [X] search · [Y] menu · LB/RB tabs · LT/RT page",
        Tab::Library => "[A] download/open · [B] actions · [X] filter · [Y] menu · ←→ fold · LB/RB tabs · LT/RT page",
        Tab::Textures => "[A] install / turn on·off · [B] actions · [Y] menu · LB/RB tabs · LT/RT page",
    }
}

fn input(f: &mut Frame, area: Rect, title: String, value: &str, editing: bool) {
    let shown = if editing { format!("{value}█") } else { value.to_string() };
    f.render_widget(Paragraph::new(shown).block(border(title, editing)), area);
}

fn draw_search(f: &mut Frame, app: &mut App, area: Rect) {
    let [inp, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    let indexing = if app.busy() { format!(" · indexing {}/{} sets…", app.indexed_sets(), app.sets.len()) } else { String::new() };
    input(
        f,
        inp,
        format!(" Find games · {} files in {} sets + live archive.org{indexing} ", app.total_files(), app.indexed_sets()),
        &app.rom_query,
        app.editing,
    );
    let items: Vec<ListItem> = app
        .rom_hits
        .iter()
        .map(|h| {
            let source = if h.live { h.source.clone() } else { h.source.chars().take(48).collect() };
            let base = h.name.rsplit('/').next().unwrap_or(&h.name);
            let on_disk = app.present.contains(&h.url) || app.on_disk.contains(&App::disk_key(&h.system, base));
            let (mark, mark_style) = if on_disk {
                ("● ", Style::new().fg(Color::Green))
            } else if app.lib.contains(&h.url) {
                ("✓ ", Style::new().fg(Color::Blue))
            } else {
                ("  ", Style::new())
            };
            let spans = vec![
                Span::styled(mark, mark_style),
                Span::raw(match &h.title {
                    Some(t) => format!("{t}  [{base}]"),
                    None => base.to_string(),
                }),
                Span::raw("  "),
                Span::styled(if h.size > 0 { human(h.size) } else { "size ?".into() }, Style::new().fg(Color::Green)),
                Span::raw("  "),
                sys_span(&h.system),
                Span::styled(format!("  {source}"), DIM),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();
    let live = if app.live_loading { " · searching archive.org…" } else { "" };
    let title = format!(" Results ({}){live} · ✓ in library · ● on disk ", app.rom_hits.len());
    f.render_stateful_widget(List::new(items).block(border(title, !app.editing)).highlight_style(HL), list, &mut app.rom_state);
}

/// (badge, color, detail) for a library game.
fn game_status(app: &App, id: &str) -> (String, Color, String) {
    if let Some(LocalState::Downloading { file, done, total }) = app.local.get(id) {
        let pct = if *total > 0 { *done as f64 / *total as f64 * 100.0 } else { 0.0 };
        let size = if *total > 0 { format!("{} / {}", human(*done), human(*total)) } else { human(*done) };
        return (format!("⇣ {pct:>3.0}%"), Color::Cyan, format!("{file}  {size}"));
    }
    let local_only = id.starts_with("local:");
    if app.needs_key.contains(id) {
        return match app.local.get(id) {
            Some(LocalState::Failed(m)) => ("✗ no key".into(), Color::Red, m.clone()),
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
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(format!("{badge:<11}"), Style::new().fg(color).bold()),
                    Span::raw(app.display_name(&g.system, &g.name)),
                ];
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
            TexRow::System { system, with_packs, on_disk } => ListItem::new(Line::from(vec![
                Span::styled(format!("▾ {}", systems::label(system)), Style::new().fg(Color::Magenta).bold()),
                Span::styled(format!("  {with_packs} of {on_disk} games on disk have texture packs"), DIM),
            ])),
            TexRow::Game(g) => {
                let g = &app.lib.games[*g];
                ListItem::new(Line::from(vec![Span::raw("  "), Span::styled(app.display_name(&g.system, &g.name), Style::new().bold())]))
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
                let mut lines = vec![Line::from(spans)];
                if !p.summary.is_empty() {
                    lines.push(Line::styled(format!("{:15}{}", "", p.summary), Style::new().fg(Color::Gray).italic()));
                }
                ListItem::new(lines)
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
        Some(p) if !p.about.is_empty() || p.status.is_some() || p.region.is_some() => {
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
            "No texture packs for the GameCube or N64 games on disk (yet). r refreshes the lists."
        };
        f.render_widget(Paragraph::new(msg).style(DIM).centered(), mid);
    }
}
