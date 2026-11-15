//! Local downloads: pull a library game from TorBox (or straight from its source)
//! into `roms_dir/<system>/`.

use crate::library::{Game, SourceKind};
use crate::systems;
use crate::torbox::TorBox;
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub enum LocalState {
    Downloading {
        file: String,
        done: u64,
        total: u64,
    },
    /// Saved files, relative to roms_dir.
    Done(Vec<String>),
    Failed(String),
}

pub type LocalTx = UnboundedSender<(String, LocalState)>;

#[derive(Clone)]
pub struct Ctx {
    pub http: reqwest::Client,
    pub torbox: Option<TorBox>,
    pub rd: Option<crate::realdebrid::RealDebrid>,
    pub roms_dir: PathBuf,
    pub poll: Duration,
}

/// Download a game locally. `via_torbox = false` fetches the source URL directly.
pub async fn fetch(game: Game, via_torbox: bool, ctx: Ctx, tx: LocalTx) {
    let id = game.id.clone();
    let send = |s| {
        let _ = tx.send((id.clone(), s));
    };
    let res = async {
        let files = match crate::ps3::needs_setup(&game, &ctx.roms_dir) {
            Some(isos) => isos,
            None if via_torbox && game.rd_id.is_some() => from_rd(&game, &ctx, &send).await?,
            None if via_torbox => from_torbox(&game, &ctx, &send).await?,
            None => direct(&game, &ctx, &send).await?,
        };
        crate::ps3::finish(files, &ctx, &send).await
    }
    .await;
    send(match res {
        Ok(files) => LocalState::Done(files),
        Err(e) => LocalState::Failed(format!("{e:#}")),
    });
}

/// Move a file the browser downloaded (Vimm) into `roms/<system>/`, then the usual unpacking.
pub async fn import_download(game: Game, src: PathBuf, ctx: Ctx, tx: LocalTx) {
    let id = game.id.clone();
    let send = |s| {
        let _ = tx.send((id.clone(), s));
    };
    let res = async {
        let file = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (system, roms) = (game.system.clone(), ctx.roms_dir.clone());
        let tx2 = tx.clone();
        let id2 = id.clone();
        let files = tokio::task::spawn_blocking(move || {
            crate::vimm::import(&src, &system, &roms, &|done, total| {
                let _ = tx2.send((id2.clone(), LocalState::Downloading { file: file.clone(), done, total }));
            })
        })
        .await??;
        crate::ps3::finish(files, &ctx, &send).await
    }
    .await;
    send(match res {
        Ok(files) => LocalState::Done(files),
        Err(e) => LocalState::Failed(format!("{e:#}")),
    });
}

async fn direct(game: &Game, ctx: &Ctx, send: &impl Fn(LocalState)) -> Result<Vec<String>> {
    if game.kind != SourceKind::Link {
        bail!("direct download only works for links, not magnets");
    }
    let rel = format!("{}/{}", game.system, sanitize(&game.name));
    ensure_dir(ctx, &game.system).await?;
    download(&ctx.http, &game.source, &ctx.roms_dir.join(&rel), send).await?;
    Ok(vec![rel])
}

/// Error text marking a TorBox copy of a Minerva folder that can't be read (the app
/// stops using TorBox for that folder for a while).
pub const BROKEN_TORBOX_ZIP: &str = "TorBox's copy is broken";

async fn from_torbox(game: &Game, ctx: &Ctx, send: &impl Fn(LocalState)) -> Result<Vec<String>> {
    let tb = ctx.torbox.as_ref().ok_or_else(|| anyhow!("no TorBox API key configured"))?;
    let kind = game.torbox_kind();
    let id = game.torbox_id.ok_or_else(|| anyhow!("not on TorBox yet"))?;
    let item = tb.get(kind, id).await?;
    if !(item.download_present || item.download_finished) || item.files.is_empty() {
        bail!("TorBox hasn't finished yet ({} {:.0}%)", item.download_state, item.progress * 100.0);
    }
    // TorBox zipped the whole torrent (account setting): read just our file out of the zip.
    if let (Some(pick), true) = (&game.pick, item.is_zipped()) {
        if game.system.is_empty() {
            bail!("no system selected; set one with s/S");
        }
        let url = tb.request_dl(kind, id, item.files[0].id).await?;
        // TorBox sometimes lists a packed zip at the folder's full size but serves only
        // the first part of it (seen: 2.5 GB of an 898 GB Saturn zip), with no directory.
        let claimed = item.files[0].size as u64;
        if let Some(served) = crate::remotezip::served_size(&ctx.http, &url).await.context("reading TorBox's zip")?
            && served < claimed
        {
            bail!("{BROKEN_TORBOX_ZIP}: its zip of this folder is cut off ({} of {})", crate::ui::human(served), crate::ui::human(claimed));
        }
        let entries = crate::remotezip::list(&ctx.http, &url)
            .await
            .map_err(|e| anyhow!("{BROKEN_TORBOX_ZIP}: {e:#}"))
            .context("reading TorBox's zip")?;
        let want = pick.to_lowercase();
        let entry = entries
            .iter()
            .find(|e| e.base_name().to_lowercase() == want)
            .ok_or_else(|| anyhow!("'{pick}' isn't in TorBox's zip of this torrent ({} entries)", entries.len()))?;
        let rel = format!("{}/{}", game.system, sanitize(pick));
        ensure_dir(ctx, &game.system).await?;
        let dest = ctx.roms_dir.join(&rel);
        if !tokio::fs::metadata(&dest).await.map(|m| m.len() > 0).unwrap_or(false) {
            let file = sanitize(pick);
            send(LocalState::Downloading { file: file.clone(), done: 0, total: entry.size });
            crate::remotezip::extract(&ctx.http, &url, entry, &dest, &|done, total| {
                send(LocalState::Downloading { file: file.clone(), done, total });
            })
            .await?;
        }
        return Ok(vec![rel]);
    }

    let mut files: Vec<_> = item.files.iter().filter(|f| !is_junk(&f.file_name())).collect();
    // One game out of a big torrent: match by file name, fall back to the torrent index.
    if let Some(pick) = &game.pick {
        let want = pick.to_lowercase();
        let found = files
            .iter()
            .find(|f| f.file_name().to_lowercase() == want)
            .or_else(|| game.torrent_index.filter(|_| !item.is_partial()).and_then(|i| files.iter().find(|f| f.id == u64::from(i))))
            .copied()
            .ok_or_else(|| anyhow!("not cached: TorBox has only {} file(s) of this torrent, and '{pick}' isn't one", item.real_files()))?;
        files = vec![found];
    }
    if files.is_empty() {
        bail!("TorBox item had no usable files");
    }

    // A single-file web download keeps the library's name so presence checks line up.
    let single_link = game.kind == SourceKind::Link && files.len() == 1;
    let item_guess = systems::guess_from_title(&item.name)
        .or_else(|| systems::guess_from_title(&game.name))
        .or_else(|| systems::vote(files.iter().map(|f| f.name.as_str())));
    let mut plan = Vec::new();
    for f in &files {
        let system = if game.system.is_empty() {
            systems::guess_for_file(&f.name)
                .or(item_guess)
                .ok_or_else(|| anyhow!("couldn't detect a system for '{}'; set one with s/S and retry", f.name))?
                .to_string()
        } else {
            game.system.clone()
        };
        let name = match &game.pick {
            Some(p) => sanitize(p),
            None if single_link => sanitize(&game.name),
            None => sanitize(&f.file_name()),
        };
        plan.push((f, format!("{system}/{name}"), system));
    }

    let mut saved = Vec::new();
    for (f, rel, system) in plan {
        ensure_dir(ctx, &system).await?;
        let url = tb.request_dl(kind, id, f.id).await?;
        download(&ctx.http, &url, &ctx.roms_dir.join(&rel), send).await?;
        saved.push(rel);
    }
    Ok(saved)
}

/// One Minerva game Real-Debrid downloaded for us (its RD torrent selects just that file).
async fn from_rd(game: &Game, ctx: &Ctx, send: &impl Fn(LocalState)) -> Result<Vec<String>> {
    let rd = ctx.rd.as_ref().ok_or_else(|| anyhow!("no Real-Debrid API key configured"))?;
    let id = game.rd_id.as_deref().ok_or_else(|| anyhow!("not on Real-Debrid yet"))?;
    let pick = game.pick.as_deref().unwrap_or(&game.name);
    let rel = format!("{}/{}", game.system, sanitize(pick));
    ensure_dir(ctx, &game.system).await?;
    let url = rd.download_url(id).await?;
    download(&ctx.http, &url, &ctx.roms_dir.join(&rel), send).await?;
    Ok(vec![rel])
}

async fn ensure_dir(ctx: &Ctx, system: &str) -> Result<PathBuf> {
    if system.is_empty() {
        bail!("no system selected; set one with s/S");
    }
    let dir = ctx.roms_dir.join(system);
    tokio::fs::create_dir_all(&dir).await.with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

pub fn is_junk(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "txt" | "nfo" | "url" | "jpg" | "png" | "sfv" | "md5" | "html")
}

pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let s: String =
        base.chars().map(|c| if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    if s.is_empty() || s == "." || s == ".." { "download.bin".into() } else { s }
}

pub async fn download(http: &reqwest::Client, url: &str, dest: &Path, send: &impl Fn(LocalState)) -> Result<()> {
    let file = dest.file_name().unwrap_or_default().to_string_lossy().to_string();
    if tokio::fs::metadata(dest).await.map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(()); // already have it
    }
    let resp = http.get(url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let part = dest.with_file_name(format!("{file}.part"));
    let mut out = tokio::fs::File::create(&part).await.with_context(|| format!("creating {}", part.display()))?;
    let mut stream = resp.bytes_stream();
    let mut done = 0u64;
    let mut last = Instant::now();
    send(LocalState::Downloading { file: file.clone(), done, total });
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        out.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if last.elapsed() > Duration::from_millis(250) {
            send(LocalState::Downloading { file: file.clone(), done, total });
            last = Instant::now();
        }
    }
    out.flush().await?;
    drop(out);
    if total > 0 && done != total {
        let _ = tokio::fs::remove_file(&part).await;
        bail!("download cut off: got {done} of {total} bytes");
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_paths() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("dir/Game: The Sequel?.zip"), "Game_ The Sequel_.zip");
        assert_eq!(sanitize(".."), "download.bin");
    }
}
