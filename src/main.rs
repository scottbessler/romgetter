mod app;
mod archive;
mod config;
mod gamepad;
mod ifarchive;
mod jobs;
mod library;
mod mame;
mod metacritic;
mod minerva;
mod ps3;
mod realdebrid;
mod remotezip;
mod sources;
mod systems;
mod textures;
mod torbox;
mod ui;
mod vimm;

use anyhow::Result;
use app::{App, Msg};
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let paths = config::Paths::new()?;
    let mut cfg = config::Config::load(&paths.config_file)?;
    let lib = library::Library::load(paths.library_file.clone())?;

    // `romgetter archive-login [email]` / `archive-logout`: keep an archive.org session so
    // restricted sets can be searched and downloaded (directly; TorBox can't log in).
    match std::env::args().nth(1).as_deref() {
        Some("archive-login") => {
            let email = match std::env::args().nth(2).or_else(|| std::env::var("ARCHIVE_ORG_EMAIL").ok()) {
                Some(e) => e,
                None => prompt("archive.org email: ", false)?,
            };
            let password = match std::env::var("ARCHIVE_ORG_PASSWORD") {
                Ok(p) if !p.is_empty() => p,
                _ => prompt("archive.org password: ", true)?,
            };
            cfg.archive_login = Some(archive::login(email.trim(), &password).await?);
            cfg.save(&paths.config_file)?;
            println!("logged in to archive.org; restricted sets now show up in Search and download directly");
            return Ok(());
        }
        Some("archive-logout") => {
            cfg.archive_login = None;
            cfg.save(&paths.config_file)?;
            println!("archive.org session removed");
            return Ok(());
        }
        _ => {}
    }

    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("romgetter/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20));
    if let Some(login) = &cfg.archive_login {
        builder = builder.cookie_provider(archive::cookie_jar(login));
    }
    let http = builder.build()?;
    let ctx = jobs::Ctx {
        torbox: cfg.api_key().map(|k| torbox::TorBox::new(http.clone(), k)),
        rd: cfg.rd_key().map(|k| realdebrid::RealDebrid::new(http.clone(), k)),
        roms_dir: cfg.roms_dir.clone(),
        poll: Duration::from_secs(cfg.poll_secs.max(2)),
        http,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();

    // `romgetter search <query...>`: headless search (no TorBox, no library changes).
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--gamepad`: read controller input from /dev/input (the Steam shortcut's launcher passes
    // it). Off by default so a session over SSH ignores the pad of whoever's holding the Deck.
    let use_gamepad = args.iter().any(|a| a == "--gamepad");
    args.retain(|a| a != "--gamepad");
    // `romgetter scan`: sync the library with the roms folder and report.
    if args.first().map(String::as_str) == Some("scan") {
        let ctx = jobs::Ctx { torbox: None, rd: None, ..ctx };
        let mut app = App::new(cfg, paths.cache_dir.clone(), lib, tx, local_tx, ctx);
        while app.scanning {
            match rx.recv().await {
                Some(m @ Msg::Scanned(_)) => app.on_msg(m),
                Some(_) => {} // ignore indexing etc.
                None => break,
            }
        }
        println!("{}", app.status);
        return Ok(());
    }

    // `romgetter textures`: which texture packs fit the GameCube/N64 games on disk (changes nothing).
    if args.first().map(String::as_str) == Some("textures") {
        let catalog = match textures::load_catalog(&paths.cache_dir) {
            Some(c) if library::now_secs().saturating_sub(c.fetched) < textures::REFRESH.as_secs() => c,
            _ => {
                let c = textures::fetch_catalog(&ctx.http).await?;
                textures::save_catalog(&paths.cache_dir, &c)?;
                c
            }
        };
        // `romgetter textures install <words>`: install the first pack whose game/author match
        if args.get(1).map(String::as_str) == Some("install") {
            let words: Vec<String> = args[2..].iter().map(|w| w.to_lowercase()).collect();
            let hit = textures::SYSTEMS.iter().find_map(|&system| {
                lib.games.iter().filter(|g| g.system == system && g.is_present(&cfg.roms_dir)).find_map(|g| {
                    let tag = g.local_paths(&cfg.roms_dir).first().and_then(|p| textures::game_tag(system, p));
                    let p = catalog.packs.iter().find(|p| {
                        let hay = format!("{} {} {}", g.name, p.author, p.name).to_lowercase();
                        textures::fits(p, system, &g.name, tag.as_deref()) && words.iter().all(|w| hay.contains(w))
                    })?;
                    Some((p.clone(), tag))
                })
            });
            let Some((pack, tag)) = hit else { anyhow::bail!("no pack for a game on disk matches {words:?}") };
            let b = pack.default_build().ok_or_else(|| anyhow::anyhow!("{} is only on a web page", pack.name))?;
            println!("installing {} / {} ({}) for {}", pack.game, pack.author, pack.builds[b].label, tag.as_deref().unwrap_or("?"));
            let dirs = textures::Dirs::for_roms(&cfg.roms_dir);
            let (stx, mut srx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(textures::get(pack.clone(), pack.builds[b].clone(), tag, dirs, ctx, move |s| {
                let _ = stx.send(s);
            }));
            while let Some(s) = srx.recv().await {
                match s {
                    textures::TexState::Done(i) => {
                        println!("done: {i:?}");
                        break;
                    }
                    textures::TexState::Failed(m) => anyhow::bail!("failed: {m}"),
                    other => println!("{other:?}"),
                }
            }
            return Ok(());
        }
        println!("{} texture packs listed", catalog.packs.len());
        for system in textures::SYSTEMS {
            for g in lib.games.iter().filter(|g| g.system == system && g.is_present(&cfg.roms_dir)) {
                let tag = g.local_paths(&cfg.roms_dir).first().and_then(|p| textures::game_tag(system, p));
                let fits: Vec<String> = catalog
                    .packs
                    .iter()
                    .filter(|p| textures::fits(p, system, &g.name, tag.as_deref()))
                    .map(|p| format!("{} [{}]", p.author, p.builds.iter().map(|b| b.label.as_str()).collect::<Vec<_>>().join(", ")))
                    .collect();
                let tag = tag.unwrap_or_else(|| "?".into());
                println!("{system} {:<55} {tag:<22} {}", g.name, if fits.is_empty() { "-".into() } else { fits.join(" · ") });
            }
        }
        return Ok(());
    }

    // `romgetter top <system>`: what `t` would add for a system (no library changes).
    if args.first().map(String::as_str) == Some("top") {
        let system = args.get(1).cloned().unwrap_or_default();
        let ctx = jobs::Ctx { torbox: None, rd: None, ..ctx };
        let app = {
            let mut app = App::new(cfg, paths.cache_dir.clone(), lib, tx, local_tx, ctx);
            while app.busy() {
                if let Some(m) = rx.recv().await {
                    app.on_msg(m);
                }
            }
            app
        };
        let titles = match metacritic::top_list(&system) {
            None => anyhow::bail!("no top-games list for '{system}'"),
            Some(metacritic::TopList::Classics(t)) => t,
            Some(metacritic::TopList::Metacritic(p)) => metacritic::top(&reqwest::Client::new(), p, 20).await?,
        };
        for (title, hit) in app.best_hits_for(&system, &titles) {
            match hit {
                Some(h) => println!("  {title:<45} → {}  [{}]", h.name.rsplit('/').next().unwrap_or(&h.name), h.source),
                None => println!("✗ {title:<45} → not found"),
            }
        }
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("search") {
        let ctx = jobs::Ctx { torbox: None, rd: None, ..ctx };
        let mut app = App::new(cfg, paths.cache_dir.clone(), lib, tx, local_tx, ctx);
        while app.busy() {
            eprint!("\rindexing {}/{} sets…", app.indexed_sets(), app.sets.len());
            if let Some(m) = rx.recv().await {
                app.on_msg(m);
            }
        }
        eprintln!("\r{} files in {} sets", app.total_files(), app.indexed_sets());
        let t0 = std::time::Instant::now();
        app.refresh_local_for(&args[1..].join(" "));
        let first = t0.elapsed();
        let t1 = std::time::Instant::now();
        app.refresh_local_for(&args[1..].join(" "));
        eprintln!("local search: {:?} per keystroke ({first:?} the first time)", t1.elapsed());
        app.search_now(&args[1..].join(" "));
        while app.live_loading {
            if let Some(m) = rx.recv().await {
                app.on_msg(m);
            }
        }
        for h in app.rom_hits.iter().take(25) {
            let base = h.name.rsplit('/').next().unwrap_or(&h.name);
            let mark = if app.on_disk.contains(&App::disk_key(&h.system, base)) { "●" } else { " " };
            let base = h.title.as_ref().map(|t| format!("{t}  [{base}]")).unwrap_or_else(|| base.to_string());
            println!("{mark} {:<12} {:>10}  {base}  [{}]", h.system, ui::human(h.size), h.source);
        }
        println!("({} results)", app.rom_hits.len());
        return Ok(());
    }

    let mut app = App::new(cfg, paths.cache_dir.clone(), lib, tx, local_tx, ctx);
    app.config_file = Some(paths.config_file.clone());

    let mut pad = if use_gamepad { gamepad::Gamepad::start() } else { gamepad::Gamepad::disabled() };
    let mut pad_was_connected = false;
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let res: Result<()> = (|| {
        while !app.quit {
            while let Ok(m) = rx.try_recv() {
                app.on_msg(m);
            }
            while let Ok((id, state)) = local_rx.try_recv() {
                app.on_msg(Msg::Local(id, state));
            }
            while let Some(p) = pad.poll() {
                app.on_pad(p);
            }
            // Show buttons when a pad shows up; keys once it's gone. In between, whichever was
            // used last wins (see App::on_event / on_pad).
            let connected = pad.connected();
            if connected != pad_was_connected {
                app.pad = connected;
                pad_was_connected = connected;
            }
            app.tick();
            terminal.draw(|f| ui::draw(f, &mut app))?;
            // Blocking poll is fine: the multi-threaded runtime keeps background tasks going.
            // Shorter with a controller, whose input arrives outside this poll.
            let wait = Duration::from_millis(if pad_was_connected { 30 } else { 100 });
            if tokio::task::block_in_place(|| event::poll(wait))? {
                app.on_event(event::read()?);
            }
        }
        Ok(())
    })();
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    res?;
    eprintln!("config: {}\nlibrary: {}", paths.config_file.display(), paths.library_file.display());
    Ok(())
}

/// Read a line from the terminal; `hidden` doesn't echo it (for passwords).
fn prompt(label: &str, hidden: bool) -> Result<String> {
    use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::crossterm::terminal;
    use std::io::Write;
    print!("{label}");
    std::io::stdout().flush()?;
    if !hidden {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_string());
    }
    terminal::enable_raw_mode()?;
    let res = (|| {
        let mut s = String::new();
        loop {
            if let Event::Key(k) = event::read()?
                && k.kind != KeyEventKind::Release
            {
                match k.code {
                    KeyCode::Enter => return Ok(s),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => anyhow::bail!("cancelled"),
                    KeyCode::Char(c) => s.push(c),
                    KeyCode::Backspace => {
                        s.pop();
                    }
                    KeyCode::Esc => anyhow::bail!("cancelled"),
                    _ => {}
                }
            }
        }
    })();
    terminal::disable_raw_mode()?;
    println!();
    res
}
