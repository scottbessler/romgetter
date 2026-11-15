//! PS3 post-processing. Redump PS3 dumps are encrypted ISOs, zipped. RPCS3 can't read a
//! zip, but it loads an encrypted ISO when the disc key sits next to it
//! (`roms/ps3/Game.iso` + `roms/ps3/Game.dkey`, the layout RetroDECK documents).
//! So after a PS3 download we unzip the ISO and fetch its key from Minerva's
//! "Disc Keys TXT" folder (one tiny zip per game, same name as the game's zip),
//! which comes through TorBox like everything else.

use crate::jobs::{Ctx, LocalState};
use crate::library::Game;
use crate::torbox::Kind;
use anyhow::{Context, Result, anyhow, bail};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const KEYS_GROUP: &str = "Redump";
/// Two copies of the same keys (hex text, and 16-byte binary); whichever TorBox has cached.
const KEYS_FOLDERS: &[&str] = &["Sony - PlayStation 3 - Disc Keys TXT", "Sony - PlayStation 3 - Disc Keys"];
/// How long to wait for TorBox to make the (cached) key torrent available.
const KEYS_WAIT: Duration = Duration::from_secs(120);

/// Systems whose emulator needs the bare disc image, not a zip (RPCS3, Xenia).
const UNZIP_SYSTEMS: &[&str] = &["ps3", "xbox360", "xbox"];

/// Disc systems whose games must be unzipped (checked against each core's source and
/// libretro core info, 2026-11):
/// - no zip support at all: NeoCD, PCSX2, Dolphin, PPSSPP, SAME CDi, Virtual Jaguar
///   (ES-DE doesn't even list .zip for most of them);
/// - Flycast (Dreamcast) treats any .zip as a Naomi/Atomiswave arcade cartridge;
/// - Beetle Saturn's zip reader is for ST-V arcade cartridges only;
/// - PUAE (Amiga CD32): unzipped to be safe;
/// - Cemu (Wii U) and Ryujinx/Eden (Switch) read no zips, and ES-DE lists no .zip for
///   them, so a zipped game doesn't even show up.
const UNZIP_ALL_SYSTEMS: &[&str] = &[
    "neogeocd",
    "neogeocdjp",
    "ps2",
    "gc",
    "wii",
    "cdimono1",
    "atarijaguarcd",
    "psp",
    "dreamcast",
    "saturn",
    "amigacd32",
    "wiiu",
    "switch",
];

/// Systems whose cores can't read zips, so RetroArch extracts one file for them: the
/// first that matches the core's extensions (libretro-common archive_file.c). A
/// single-file zip works as-is; a .cue plus .bin tracks doesn't (the tracks stay in the
/// zip), so only multi-file zips get unpacked.
const UNZIP_MULTI_FILE_SYSTEMS: &[&str] = &["3do", "psx", "segacd", "pcenginecd", "pcfx"];

/// Files in a local zip (0 if it can't be read).
fn zip_file_count(path: &Path) -> usize {
    let Ok(f) = std::fs::File::open(path) else { return 0 };
    let Ok(mut z) = zip::ZipArchive::new(std::io::BufReader::new(f)) else { return 0 };
    (0..z.len()).filter(|&i| z.by_index_raw(i).is_ok_and(|e| e.is_file())).count()
}

fn in_systems(rel: &str, systems: &[&str]) -> bool {
    systems.iter().any(|s| rel.starts_with(&format!("{s}/")))
}

fn is_zip(rel: &str) -> bool {
    rel.to_lowercase().ends_with(".zip")
}

fn is_disc_zip(rel: &str) -> bool {
    in_systems(rel, UNZIP_SYSTEMS) && is_zip(rel)
}

/// A downloaded zip (`rel` under `roms_dir`) that has to be unpacked whole.
fn is_unzip_all(rel: &str, roms_dir: &Path) -> bool {
    is_zip(rel)
        && (in_systems(rel, UNZIP_ALL_SYSTEMS) || (in_systems(rel, UNZIP_MULTI_FILE_SYSTEMS) && zip_file_count(&roms_dir.join(rel)) > 1))
}

/// Whether a local zip holds an ISO (reads only its directory).
fn zip_has_iso(path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(path) else { return false };
    let Ok(mut z) = zip::ZipArchive::new(std::io::BufReader::new(f)) else { return false };
    (0..z.len()).any(|i| z.by_index_raw(i).is_ok_and(|e| e.name().to_lowercase().ends_with(".iso")))
}

fn with_ext(rel: &str, ext: &str) -> String {
    let stem = rel.rsplit_once('.').map(|(s, _)| s).unwrap_or(rel);
    format!("{stem}.{ext}")
}

fn local_rels(game: &Game, roms_dir: &Path) -> Vec<String> {
    game.local_paths(roms_dir)
        .iter()
        .filter_map(|p| p.strip_prefix(roms_dir).ok())
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .collect()
}

/// A PS3 / Xbox 360 game already on disk that still needs work before the emulator can
/// run it: a zip to unpack, or (PS3) an ISO without its `.dkey`. Returns its files for `finish`. Also covers an
/// ISO unpacked from the zip the library still points at (the zip is deleted after
/// unpacking), so a retry never downloads the whole disc again.
pub fn needs_setup(game: &Game, roms_dir: &Path) -> Option<Vec<String>> {
    if UNZIP_ALL_SYSTEMS.contains(&game.system.as_str()) || UNZIP_MULTI_FILE_SYSTEMS.contains(&game.system.as_str()) {
        let rels = local_rels(game, roms_dir);
        let ready = !rels.is_empty() && rels.iter().all(|r| roms_dir.join(r).is_file());
        return (ready && rels.iter().any(|r| is_unzip_all(r, roms_dir))).then_some(rels);
    }
    if !UNZIP_SYSTEMS.contains(&game.system.as_str()) {
        return None;
    }
    let rels: Vec<String> = local_rels(game, roms_dir)
        .into_iter()
        .map(|r| {
            let iso = with_ext(&r, "iso");
            if is_disc_zip(&r) && !roms_dir.join(&r).exists() && roms_dir.join(&iso).is_file() { iso } else { r }
        })
        .collect();
    if rels.is_empty() || !rels.iter().all(|r| roms_dir.join(r).exists()) {
        return None;
    }
    let needs_key = |r: &str| is_iso(r) && !roms_dir.join(with_ext(r, "dkey")).is_file() && iso_decrypted(&roms_dir.join(r)) != Some(true);
    let xbox_redump = |r: &str| r.starts_with("xbox/") && r.to_lowercase().ends_with(".iso") && is_xbox_redump(&roms_dir.join(r));
    let pending = rels.iter().any(|r| (is_disc_zip(r) && zip_has_iso(&roms_dir.join(r))) || needs_key(r) || xbox_redump(r));
    pending.then_some(rels)
}

fn is_iso(rel: &str) -> bool {
    rel.starts_with("ps3/") && rel.to_lowercase().ends_with(".iso")
}

/// Unzip PS3 / Xbox 360 ISOs and add PS3 disc keys. Other files pass through unchanged.
pub async fn finish(files: Vec<String>, ctx: &Ctx, send: &impl Fn(LocalState)) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for rel in files {
        if is_unzip_all(&rel, &ctx.roms_dir) {
            out.extend(unpack_all(&rel, ctx, send).await?);
            continue;
        }
        let rel = if is_disc_zip(&rel) { unpack(&rel, ctx, send).await? } else { rel };
        // A full Redump Xbox ISO that came unzipped (or predates this): trim it for xemu.
        if rel.starts_with("xbox/") && rel.to_lowercase().ends_with(".iso") && is_xbox_redump(&ctx.roms_dir.join(&rel)) {
            trim_xbox_iso(&rel, ctx, send).await?;
        }
        if is_iso(&rel) {
            let key_path = ctx.roms_dir.join(with_ext(&rel, "dkey"));
            // Already-decrypted ISOs (common outside Redump) need no key.
            if !key_path.is_file() && iso_decrypted(&ctx.roms_dir.join(&rel)) != Some(true) {
                let stem = rel.rsplit('/').next().unwrap_or(&rel);
                let stem = stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem).to_string();
                send(LocalState::Downloading { file: format!("disc key for {stem}"), done: 0, total: 0 });
                let key = fetch_key(ctx, &stem).await.context("ISO saved, but getting its disc key failed (Enter retries)")?;
                tokio::fs::write(&key_path, format!("{key}\n")).await.with_context(|| format!("writing {}", key_path.display()))?;
            }
        }
        // The .dkey isn't listed as a game file: the roms-folder scan ignores keys, and a
        // missing key shows as "needs key" (see needs_setup), not as a missing game.
        out.push(rel);
    }
    Ok(out)
}

/// Extract the .iso from a downloaded PS3 zip next to it, then delete the zip.
/// Returns the ISO's path relative to roms_dir; a zip without an ISO is kept as-is.
async fn unpack(rel: &str, ctx: &Ctx, send: &impl Fn(LocalState)) -> Result<String> {
    let zip_path = ctx.roms_dir.join(rel);
    let iso_rel = with_ext(rel, "iso");
    let iso_path = ctx.roms_dir.join(&iso_rel);
    let label = format!("unzipping {}", iso_rel.rsplit('/').next().unwrap_or(&iso_rel));
    let done = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let task = {
        let (zip_path, iso_path, done, total) = (zip_path.clone(), iso_path.clone(), done.clone(), total.clone());
        tokio::task::spawn_blocking(move || extract_iso(&zip_path, &iso_path, &done, &total))
    };
    tokio::pin!(task);
    let found = loop {
        tokio::select! {
            res = &mut task => break res??,
            () = tokio::time::sleep(Duration::from_millis(250)) => {
                send(LocalState::Downloading { file: label.clone(), done: done.load(Ordering::Relaxed), total: total.load(Ordering::Relaxed) });
            }
        }
    };
    if !found {
        return Ok(rel.to_string());
    }
    tokio::fs::remove_file(&zip_path).await.with_context(|| format!("removing {}", zip_path.display()))?;
    Ok(iso_rel)
}

/// Unzip every file of a downloaded zip into its folder (flat), then delete the zip.
/// Returns the extracted files relative to roms_dir.
async fn unpack_all(rel: &str, ctx: &Ctx, send: &impl Fn(LocalState)) -> Result<Vec<String>> {
    let zip_path = ctx.roms_dir.join(rel);
    let dir_rel = rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let label = format!("unzipping {}", rel.rsplit('/').next().unwrap_or(rel));
    let done = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let task = {
        let (zip_path, dir, done, total) = (zip_path.clone(), ctx.roms_dir.join(&dir_rel), done.clone(), total.clone());
        tokio::task::spawn_blocking(move || extract_all(&zip_path, &dir, &done, &total))
    };
    tokio::pin!(task);
    let names = loop {
        tokio::select! {
            res = &mut task => break res??,
            () = tokio::time::sleep(Duration::from_millis(250)) => {
                send(LocalState::Downloading { file: label.clone(), done: done.load(Ordering::Relaxed), total: total.load(Ordering::Relaxed) });
            }
        }
    };
    if names.is_empty() {
        return Ok(vec![rel.to_string()]);
    }
    tokio::fs::remove_file(&zip_path).await.with_context(|| format!("removing {}", zip_path.display()))?;
    Ok(names.into_iter().map(|n| format!("{dir_rel}/{n}")).collect())
}

/// Blocking: extract every file entry of a zip into `dir` (folders inside the zip are
/// flattened). Each file is written as `.part` first. Returns the file names written.
fn extract_all(zip_path: &Path, dir: &Path, done: &AtomicU64, total: &AtomicU64) -> Result<Vec<String>> {
    let file = std::fs::File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).context("reading the zip")?;
    let mut entries = Vec::new();
    for i in 0..zip.len() {
        let e = zip.by_index_raw(i)?;
        let name = e.enclosed_name().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
        if let (true, Some(name)) = (e.is_file(), name) {
            entries.push((i, name, e.size()));
        }
    }
    total.store(entries.iter().map(|(_, _, s)| s).sum(), Ordering::Relaxed);
    let mut written = Vec::new();
    for (i, name, _) in entries {
        let dest = dir.join(&name);
        let part = dir.join(format!("{name}.part"));
        let mut entry = zip.by_index(i)?;
        let mut out = std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
            done.fetch_add(n as u64, Ordering::Relaxed);
        }
        drop(out);
        std::fs::rename(&part, &dest)?;
        written.push(name);
    }
    Ok(written)
}

/// Blocking: copy the zip's (largest) .iso entry to `dest`. false = the zip has no ISO.
fn extract_iso(zip_path: &Path, dest: &Path, done: &AtomicU64, total: &AtomicU64) -> Result<bool> {
    let file = std::fs::File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).context("reading the PS3 zip")?;
    let mut best: Option<(usize, u64)> = None;
    for i in 0..zip.len() {
        let e = zip.by_index_raw(i)?;
        if e.is_file() && e.name().to_lowercase().ends_with(".iso") && best.is_none_or(|(_, s)| e.size() > s) {
            best = Some((i, e.size()));
        }
    }
    let Some((i, size)) = best else { return Ok(false) };
    total.store(size, Ordering::Relaxed);
    let entry = zip.by_index(i)?;
    let xbox = dest.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "xbox");
    copy_disc(entry, dest, xbox.then_some(XBOX_GAME_OFFSET), done)?;
    Ok(true)
}

/// Where an original-Xbox game partition starts in a Redump image (after the video
/// partition), and the XDVDFS header that marks a game partition 64 KiB in.
const XBOX_GAME_OFFSET: u64 = 387 * 1024 * 1024;
const XDVDFS_AT: usize = 0x10000;
const XDVDFS_MAGIC: &[u8] = b"MICROSOFT*XBOX*MEDIA";

/// Copy a disc image to `dest` (via a .part file). With `xbox` (the game partition's
/// offset), a full Redump image (video + game partition) is written as just the game
/// partition, which is what xemu loads; one that already is that is copied as-is.
fn copy_disc(mut src: impl Read, dest: &Path, xbox: Option<u64>, done: &AtomicU64) -> Result<()> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let mut out =
        std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?);
    let mut buf = vec![0u8; 1 << 20];
    let mut read_total = 0u64;
    // Bytes still to drop from the front (the Xbox video partition), once we know.
    let mut skip = 0u64;
    if let Some(offset) = xbox {
        let mut head = vec![0u8; XDVDFS_AT + XDVDFS_MAGIC.len()];
        src.read_exact(&mut head).context("reading the disc image")?;
        read_total = head.len() as u64;
        if &head[XDVDFS_AT..] == XDVDFS_MAGIC {
            std::io::Write::write_all(&mut out, &head)?; // already just the game partition
        } else {
            skip = offset - read_total;
        }
        done.store(read_total, Ordering::Relaxed);
    }
    loop {
        let n = src.read(&mut buf).context("reading the disc image")?;
        if n == 0 {
            break;
        }
        read_total += n as u64;
        let drop_now = skip.min(n as u64) as usize;
        skip -= drop_now as u64;
        std::io::Write::write_all(&mut out, &buf[drop_now..n])?;
        done.store(read_total, Ordering::Relaxed);
    }
    std::io::Write::flush(&mut out)?;
    drop(out);
    if xbox.is_some() && !is_xbox_game_partition(&part) {
        let _ = std::fs::remove_file(&part);
        bail!("this doesn't look like an original Xbox disc image (no game partition found)");
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// Whether a file starts with an Xbox game partition (what xemu needs).
fn is_xbox_game_partition(path: &Path) -> bool {
    has_magic_at(path, XDVDFS_AT as u64)
}

/// A full Redump Xbox image (video partition first), which xemu can't load.
fn is_xbox_redump(path: &Path) -> bool {
    !is_xbox_game_partition(path) && has_magic_at(path, XBOX_GAME_OFFSET + XDVDFS_AT as u64)
}

fn has_magic_at(path: &Path, at: u64) -> bool {
    use std::io::{Seek, SeekFrom};
    let mut b = vec![0u8; XDVDFS_MAGIC.len()];
    std::fs::File::open(path)
        .and_then(|mut f| {
            f.seek(SeekFrom::Start(at))?;
            f.read_exact(&mut b)
        })
        .is_ok()
        && b == XDVDFS_MAGIC
}

/// Rewrite a full Redump Xbox ISO on disk as just its game partition.
async fn trim_xbox_iso(rel: &str, ctx: &Ctx, send: &impl Fn(LocalState)) -> Result<()> {
    let path = ctx.roms_dir.join(rel);
    let total = std::fs::metadata(&path)?.len();
    let label = format!("converting {} for xemu", rel.rsplit('/').next().unwrap_or(rel));
    let done = Arc::new(AtomicU64::new(0));
    let task = {
        let (path, done) = (path.clone(), done.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let tmp = path.with_extension("xiso");
            copy_disc(std::io::BufReader::new(std::fs::File::open(&path)?), &tmp, Some(XBOX_GAME_OFFSET), &done)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })
    };
    tokio::pin!(task);
    loop {
        tokio::select! {
            res = &mut task => return res?,
            () = tokio::time::sleep(Duration::from_millis(250)) => {
                send(LocalState::Downloading { file: label.clone(), done: done.load(Ordering::Relaxed), total });
            }
        }
    }
}

/// The disc key (32 hex chars) for a Redump PS3 game, e.g. "God of War III (USA)".
async fn fetch_key(ctx: &Ctx, stem: &str) -> Result<String> {
    let tb = ctx.torbox.as_ref().ok_or_else(|| anyhow!("PS3 disc keys come through TorBox; no API key configured"))?;
    let mut want = String::new();
    let mut folders = Vec::new();
    for name in KEYS_FOLDERS {
        let folder =
            crate::minerva::fetch_folder(&ctx.http, KEYS_GROUP, name).await.with_context(|| format!("listing Minerva's {name}"))?;
        let Some(btih) = folder.btih.clone() else { continue };
        let names: Vec<&str> = folder.files.iter().map(|f| f.name.as_str()).collect();
        if let Some(key_name) = key_file_for(stem, &names)? {
            want = key_name.to_lowercase();
            let f = folder.files.iter().find(|f| f.name.to_lowercase() == want);
            folders.push((*name, btih, f.and_then(|f| f.torrent_index)));
        }
    }
    if folders.is_empty() {
        bail!("no disc key for '{stem}' on Minerva");
    }

    // The key torrents are shared by every PS3 game: reuse one already on TorBox, else add
    // a cached one, else let TorBox download it (they're about a megabyte).
    let mine = tb.list(Kind::Torrent).await?;
    let mut picked = folders.iter().find_map(|(n, h, i)| mine.iter().find(|t| t.hash.eq_ignore_ascii_case(h)).map(|t| (t.id, *n, *i)));
    if picked.is_none() {
        for only_cached in [true, false] {
            let mut last_err = None;
            for (n, h, i) in &folders {
                match tb.create(Kind::Torrent, &crate::minerva::magnet(h), Some(n), only_cached).await {
                    Ok(id) => {
                        picked = Some((id, *n, *i));
                        break;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            if picked.is_some() {
                break;
            }
            if !only_cached {
                return Err(last_err.unwrap()).context("adding a PS3 disc key torrent to TorBox");
            }
        }
    }
    let (id, folder_name, torrent_index) = picked.unwrap();
    let start = Instant::now();
    let item = loop {
        let item = tb.get(Kind::Torrent, id).await?;
        if (item.download_present || item.download_finished) && !item.files.is_empty() {
            break item;
        }
        if start.elapsed() > KEYS_WAIT {
            bail!("TorBox is still fetching the PS3 disc keys ({folder_name}, {} {:.0}%)", item.download_state, item.progress * 100.0);
        }
        tokio::time::sleep(ctx.poll.min(Duration::from_secs(5))).await;
    };

    let zipped_key = if item.is_zipped() {
        // TorBox packed the folder into one zip; pull out just our game's key zip.
        let url = tb.request_dl(Kind::Torrent, id, item.files[0].id).await?;
        let entries = crate::remotezip::list(&ctx.http, &url).await.context("reading TorBox's zip of the disc keys")?;
        let entry = entries
            .iter()
            .find(|e| e.base_name().to_lowercase() == want)
            .ok_or_else(|| anyhow!("'{stem}.zip' isn't in TorBox's zip of the disc keys"))?;
        let tmp = std::env::temp_dir().join(format!("romgetter-key-{}-{}", std::process::id(), entry.local_header));
        crate::remotezip::extract(&ctx.http, &url, entry, &tmp, &|_, _| {}).await?;
        let bytes = tokio::fs::read(&tmp).await?;
        let _ = tokio::fs::remove_file(&tmp).await;
        bytes
    } else {
        let f = item
            .files
            .iter()
            .find(|f| f.file_name().to_lowercase() == want)
            .or_else(|| torrent_index.and_then(|i| item.files.iter().find(|f| f.id == u64::from(i))))
            .ok_or_else(|| anyhow!("'{stem}.zip' isn't in the TorBox disc key torrent"))?;
        let url = tb.request_dl(Kind::Torrent, id, f.id).await?;
        ctx.http.get(&url).send().await?.error_for_status()?.bytes().await?.to_vec()
    };
    key_from_zip(&zipped_key)
}

/// The key file for a game: its exact Redump name, or else (for ISOs named like
/// "X-Com Enemy Unknown [BLUS30943]") the one key whose title matches ignoring spaces and
/// punctuation, in the region the serial says. Several matches = ambiguous (an error).
fn key_file_for(stem: &str, names: &[&str]) -> Result<Option<String>> {
    let want = format!("{stem}.zip").to_lowercase();
    if let Some(n) = names.iter().find(|n| n.to_lowercase() == want) {
        return Ok(Some(n.to_string()));
    }
    let compact = |s: &str| crate::metacritic::norm(s).replace(' ', "");
    let title = compact(stem);
    if title.is_empty() {
        return Ok(None);
    }
    let region = serial_region(stem);
    let hits: Vec<&&str> = names
        .iter()
        .filter(|n| compact(n) == title)
        .filter(|n| region.is_none_or(|r| n.split('(').skip(1).any(|t| t.split(')').next().unwrap_or("").contains(r))))
        .collect();
    match hits.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.to_string())),
        many => bail!(
            "several disc keys could match '{stem}' ({}); rename the ISO to its Redump name",
            many.iter().map(|n| n.trim_end_matches(".zip")).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Region from a PS3 serial in the name ("[BLUS30943]" → "USA").
fn serial_region(name: &str) -> Option<&'static str> {
    let up = name.to_uppercase();
    let serial = up.split(|c: char| !c.is_ascii_alphanumeric()).find(|w| w.len() == 9 && w[4..].chars().all(|c| c.is_ascii_digit()))?;
    match &serial[..4] {
        "BLUS" | "BCUS" | "NPUB" | "NPUA" => Some("USA"),
        "BLES" | "BCES" | "NPEB" | "NPEA" => Some("Europe"),
        "BLJM" | "BLJS" | "BCJS" | "BCJM" | "NPJB" | "NPJA" => Some("Japan"),
        "BLAS" | "BCAS" | "NPHB" | "NPHA" => Some("Asia"),
        "BLKS" | "BCKS" | "NPKB" => Some("Korea"),
        _ => None,
    }
}

/// Whether a PS3 ISO is already decrypted: its `PS3_GAME/USRDIR/EBOOT.BIN` starts with
/// the plain "SCE" header. Those (common in non-Redump uploads) need no disc key.
/// None = couldn't tell (not a PS3 game ISO, unreadable). Cached by path + size.
pub fn iso_decrypted(path: &Path) -> Option<bool> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    /// (path, size) → decrypted?
    type Seen = Mutex<HashMap<(PathBuf, u64), Option<bool>>>;
    static SEEN: OnceLock<Seen> = OnceLock::new();
    let size = std::fs::metadata(path).ok()?.len();
    let key = (path.to_path_buf(), size);
    let seen = SEEN.get_or_init(Default::default);
    if let Some(v) = seen.lock().ok()?.get(&key) {
        return *v;
    }
    let v = read_eboot_magic(path).ok().map(|m| &m == b"SCE\0");
    seen.lock().ok()?.insert(key, v);
    v
}

fn read_eboot_magic(path: &Path) -> Result<[u8; 4]> {
    use std::io::{Seek, SeekFrom};
    const S: u64 = 2048;
    let mut f = std::fs::File::open(path)?;
    let mut read = |at: u64, len: usize| -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(&mut buf)?;
        Ok(buf)
    };
    let pvd = read(16 * S, S as usize)?;
    if &pvd[1..6] != b"CD001" {
        bail!("not an ISO9660 image");
    }
    let le = |b: &[u8], at: usize| u64::from(u32::from_le_bytes(b[at..at + 4].try_into().unwrap()));
    let (mut lba, mut size) = (le(&pvd, 158), le(&pvd, 166));
    for part in ["PS3_GAME", "USRDIR", "EBOOT.BIN"] {
        let dir = read(lba * S, size.min(64 * S) as usize)?;
        let mut i = 0usize;
        let mut found = None;
        while i < dir.len() {
            let len = dir[i] as usize;
            if len == 0 {
                i = (i / S as usize + 1) * S as usize; // records don't cross sectors
                continue;
            }
            let name_len = dir[i + 32] as usize;
            let name = String::from_utf8_lossy(&dir[i + 33..(i + 33 + name_len).min(dir.len())]).to_uppercase();
            if name.split(';').next() == Some(part) {
                found = Some((le(&dir, i + 2), le(&dir, i + 10)));
                break;
            }
            i += len;
        }
        (lba, size) = found.ok_or_else(|| anyhow!("{part} not found"))?;
    }
    let magic = read(lba * S, 4)?;
    Ok(magic.try_into().unwrap())
}

/// A key zip holds one small file: the key as hex text, or 16 raw bytes.
fn key_from_zip(bytes: &[u8]) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("reading the disc key zip")?;
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        if !e.is_file() {
            continue;
        }
        let mut data = Vec::new();
        e.read_to_end(&mut data)?;
        if let Some(k) = parse_key(&data) {
            return Ok(k);
        }
    }
    bail!("the disc key zip didn't contain a key")
}

fn parse_key(data: &[u8]) -> Option<String> {
    if data.len() == 16 {
        return Some(data.iter().fold(String::with_capacity(32), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02X}");
            s
        }));
    }
    let text = std::str::from_utf8(data).ok()?;
    let hex: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    (hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| hex.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in files {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn parses_keys() {
        assert_eq!(parse_key(b"0123456789abcdef0123456789ABCDEF\r\n").as_deref(), Some("0123456789ABCDEF0123456789ABCDEF"));
        assert_eq!(parse_key(&[0xAB; 16]).as_deref(), Some("ABABABABABABABABABABABABABABABAB"));
        assert_eq!(parse_key(b"not a key"), None);
        let z = zip_of(&[("Game (USA).txt", b"00112233445566778899aabbccddeeff")]);
        assert_eq!(key_from_zip(&z).unwrap(), "00112233445566778899AABBCCDDEEFF");
    }

    #[test]
    fn unzips_iso() {
        let dir = std::env::temp_dir().join(format!("romgetter-ps3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("Game (USA).zip");
        std::fs::write(&zip_path, zip_of(&[("readme.txt", b"x"), ("Game (USA).iso", &[7u8; 5000])])).unwrap();
        let (d, t) = (AtomicU64::new(0), AtomicU64::new(0));
        assert!(extract_iso(&zip_path, &dir.join("Game (USA).iso"), &d, &t).unwrap());
        assert_eq!(std::fs::read(dir.join("Game (USA).iso")).unwrap(), vec![7u8; 5000]);
        assert_eq!(d.load(Ordering::Relaxed), 5000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unzips_whole_cd_games() {
        let dir = std::env::temp_dir().join(format!("romgetter-unzip-all-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("neogeocd")).unwrap();
        std::fs::write(
            dir.join("neogeocd/Game (USA).zip"),
            zip_of(&[
                ("Game (USA).cue", b"FILE"),
                ("sub/Game (USA) (Track 1).bin", &[1u8; 300]),
                ("Game (USA) (Track 2).bin", &[2u8; 200]),
            ]),
        )
        .unwrap();
        let g = Game::torrent_file("m".into(), "Game (USA).zip".into(), "neogeocd".into(), "magnet:?".into(), None, 1);
        assert_eq!(needs_setup(&g, &dir), Some(vec!["neogeocd/Game (USA).zip".to_string()]));
        // Wii U: Cemu and ES-DE take no zips, even a single-file one
        std::fs::create_dir_all(dir.join("wiiu")).unwrap();
        std::fs::write(dir.join("wiiu/Game (USA).zip"), zip_of(&[("Game (USA).wux", &[3u8; 100])])).unwrap();
        let wiiu = Game::torrent_file("w".into(), "Game (USA).zip".into(), "wiiu".into(), "magnet:?".into(), None, 1);
        assert_eq!(needs_setup(&wiiu, &dir), Some(vec!["wiiu/Game (USA).zip".to_string()]));
        // 3DO / PS1: RetroArch extracts one file, so only multi-file zips need unpacking
        std::fs::create_dir_all(dir.join("3do")).unwrap();
        std::fs::write(dir.join("3do/One (USA).zip"), zip_of(&[("One (USA).iso", b"x")])).unwrap();
        std::fs::write(dir.join("3do/Two (USA).zip"), zip_of(&[("Two (USA).cue", b"x"), ("Two (USA).bin", b"y")])).unwrap();
        let one = Game::torrent_file("1".into(), "One (USA).zip".into(), "3do".into(), "magnet:?".into(), None, 1);
        let two = Game::torrent_file("2".into(), "Two (USA).zip".into(), "3do".into(), "magnet:?".into(), None, 1);
        assert_eq!(needs_setup(&one, &dir), None);
        assert_eq!(needs_setup(&two, &dir), Some(vec!["3do/Two (USA).zip".to_string()]));
        let (d, t) = (AtomicU64::new(0), AtomicU64::new(0));
        let names = extract_all(&dir.join("neogeocd/Game (USA).zip"), &dir.join("neogeocd"), &d, &t).unwrap();
        assert_eq!(names, ["Game (USA).cue", "Game (USA) (Track 1).bin", "Game (USA) (Track 2).bin"]);
        assert_eq!(std::fs::read(dir.join("neogeocd/Game (USA) (Track 1).bin")).unwrap(), vec![1u8; 300]);
        assert_eq!(t.load(Ordering::Relaxed), 504);
        assert_eq!(d.load(Ordering::Relaxed), 504);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flags_isos_without_keys() {
        let dir = std::env::temp_dir().join(format!("romgetter-ps3-setup-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("ps3")).unwrap();
        std::fs::write(dir.join("ps3/Game (USA).iso"), b"x").unwrap();
        // found by the roms-folder scan, no key yet
        let local = Game::local("ps3", "ps3/Game (USA).iso".into(), 1);
        assert_eq!(needs_setup(&local, &dir), Some(vec!["ps3/Game (USA).iso".to_string()]));
        // downloaded from Minerva: the library points at the zip, which unpacking deleted
        let mut dl = Game::torrent_file("m".into(), "Game (USA).zip".into(), "ps3".into(), "magnet:?".into(), None, 1);
        assert_eq!(needs_setup(&dl, &dir), Some(vec!["ps3/Game (USA).iso".to_string()]));
        std::fs::write(dir.join("ps3/Game (USA).dkey"), b"00").unwrap();
        assert_eq!(needs_setup(&local, &dir), None);
        assert_eq!(needs_setup(&dl, &dir), None);
        // a zip not unpacked yet
        dl.pick = Some("Other (USA).zip".into());
        std::fs::write(dir.join("ps3/Other (USA).zip"), zip_of(&[("Other (USA).iso", b"x")])).unwrap();
        assert_eq!(needs_setup(&dl, &dir), Some(vec!["ps3/Other (USA).zip".to_string()]));
        assert_eq!(needs_setup(&Game::local("ps2", "ps2/Game (USA).iso".into(), 1), &dir), None);
        // Xbox 360: a zip with an ISO needs unzipping; one without (e.g. a GoD folder) doesn't
        std::fs::create_dir_all(dir.join("xbox360")).unwrap();
        std::fs::write(dir.join("xbox360/Blitz (USA).zip"), zip_of(&[("Blitz (USA).iso", b"x")])).unwrap();
        std::fs::write(dir.join("xbox360/God (USA).zip"), zip_of(&[("God/data0000", b"x")])).unwrap();
        assert!(needs_setup(&Game::local("xbox360", "xbox360/Blitz (USA).zip".into(), 1), &dir).is_some());
        assert_eq!(needs_setup(&Game::local("xbox360", "xbox360/God (USA).zip".into(), 1), &dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A tiny ISO9660 image with PS3_GAME/USRDIR/EBOOT.BIN starting with `eboot`.
    fn fake_iso(path: &Path, eboot: &[u8]) {
        const S: usize = 2048;
        let mut img = vec![0u8; 24 * S];
        let record = |name: &str, lba: u32, dir: bool| {
            let mut r = vec![0u8; 34 + name.len()];
            r[0] = r.len() as u8;
            r[2..6].copy_from_slice(&lba.to_le_bytes());
            r[10..14].copy_from_slice(&(S as u32).to_le_bytes());
            r[25] = if dir { 2 } else { 0 };
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name.as_bytes());
            r
        };
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"CD001");
        img[16 * S + 156..16 * S + 156 + 34].copy_from_slice(&record("\0", 20, true)[..34]);
        for (sector, rec) in [(20, record("PS3_GAME", 21, true)), (21, record("USRDIR", 22, true)), (22, record("EBOOT.BIN;1", 23, false))]
        {
            img[sector * S..sector * S + rec.len()].copy_from_slice(&rec);
        }
        img[23 * S..23 * S + eboot.len()].copy_from_slice(eboot);
        std::fs::write(path, img).unwrap();
    }

    #[test]
    fn spots_decrypted_isos() {
        let dir = std::env::temp_dir().join(format!("romgetter-iso-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        fake_iso(&dir.join("dec.iso"), b"SCE\0\0\0\0\x02");
        fake_iso(&dir.join("enc.iso"), &[0x3c, 0xad, 0x29, 0x3f]);
        std::fs::write(dir.join("junk.iso"), b"not an iso").unwrap();
        assert_eq!(iso_decrypted(&dir.join("dec.iso")), Some(true));
        assert_eq!(iso_decrypted(&dir.join("enc.iso")), Some(false));
        assert_eq!(iso_decrypted(&dir.join("junk.iso")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_keys_for_serial_named_isos() {
        let names = [
            "XCOM - Enemy Unknown (USA).zip",
            "XCOM - Enemy Unknown (Europe) (En,Fr,De,Es,It).zip",
            "Blitz - The League II (USA) (v01.01).zip",
            "Batman - Arkham City (USA).zip",
            "Batman - Arkham City (USA) (Rev 1).zip",
        ];
        let got = |stem: &str| key_file_for(stem, &names);
        assert_eq!(got("Blitz - The League II (USA) (v01.01)").unwrap().as_deref(), Some("Blitz - The League II (USA) (v01.01).zip"));
        assert_eq!(got("X-Com Enemy Unknown [BLUS30943]").unwrap().as_deref(), Some("XCOM - Enemy Unknown (USA).zip"));
        assert_eq!(got("X-Com Enemy Unknown [BLES01724]").unwrap().as_deref(), Some("XCOM - Enemy Unknown (Europe) (En,Fr,De,Es,It).zip"));
        assert!(got("Batman Arkham City [BLUS30607]").is_err(), "two USA versions: ambiguous");
        assert_eq!(got("Some Other Game [BLUS99999]").unwrap(), None);
    }

    #[test]
    fn trims_xbox_redump_images_to_the_game_partition() {
        let dir = std::env::temp_dir().join(format!("romgetter-xbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let offset = 300_000u64; // stands in for the 387 MiB video partition
        let game = {
            let mut g = vec![7u8; XDVDFS_AT + 5000];
            g[XDVDFS_AT..XDVDFS_AT + XDVDFS_MAGIC.len()].copy_from_slice(XDVDFS_MAGIC);
            g
        };
        let redump: Vec<u8> = std::iter::repeat_n(1u8, offset as usize).chain(game.iter().copied()).collect();
        let done = AtomicU64::new(0);
        copy_disc(&redump[..], &dir.join("a.iso"), Some(offset), &done).unwrap();
        assert_eq!(std::fs::read(dir.join("a.iso")).unwrap(), game, "video partition dropped");
        assert_eq!(done.load(Ordering::Relaxed), redump.len() as u64);
        copy_disc(&game[..], &dir.join("b.iso"), Some(offset), &done).unwrap();
        assert_eq!(std::fs::read(dir.join("b.iso")).unwrap(), game, "already a game partition: unchanged");
        assert!(copy_disc(&vec![0u8; 400_000][..], &dir.join("c.iso"), Some(offset), &done).is_err(), "not an Xbox disc");
        assert!(!dir.join("c.iso").exists() && !dir.join("c.iso.part").exists());
        assert!(is_xbox_game_partition(&dir.join("a.iso")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rel_helpers() {
        assert!(is_disc_zip("ps3/God of War III (USA).zip"));
        assert!(!is_disc_zip("ps2/God of War (USA).zip"));
        assert_eq!(with_ext("ps3/A (USA) (v1.01).zip", "iso"), "ps3/A (USA) (v1.01).iso");
    }

    #[tokio::test]
    #[ignore = "network + TorBox key from the config"]
    async fn ps3_key_live() {
        let paths = crate::config::Paths::new().unwrap();
        let cfg = crate::config::Config::load(&paths.config_file).unwrap();
        let key = cfg.api_key().expect("torbox key");
        let http = reqwest::Client::new();
        let ctx = Ctx {
            http: http.clone(),
            rd: None,
            torbox: Some(crate::torbox::TorBox::new(http, key)),
            roms_dir: std::env::temp_dir(),
            poll: Duration::from_secs(3),
        };
        let game = std::env::var("PS3_KEY_GAME").unwrap_or_else(|_| "007 - Blood Stone (USA) (En,Fr)".into());
        let k = fetch_key(&ctx, &game).await.unwrap();
        println!("{k}");
        assert_eq!(k.len(), 32);
    }
}
