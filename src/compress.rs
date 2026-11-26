//! Disc games stored the way their emulators read them directly: CHD for CD and DVD
//! systems, RVZ for GameCube / Wii. Both are smaller than a zip (CHD: often much smaller
//! than the raw cue/bin), and nothing has to unzip at launch (RetroArch otherwise
//! extracts the whole image to a temp folder each time).
//!
//! The tools come from RetroDECK, which ships MAME's `chdman` and Dolphin's
//! `dolphin-tool` inside its flatpak (they're run through `flatpak run`, with the
//! library path the component's own launcher sets). One on PATH is used first. Without
//! a tool, games are simply unzipped as before.
//!
//! Which default emulator reads what (RetroDECK 0.11's es_systems.xml, 2026-11):
//! CHD: SwanStation, Genesis Plus GX, Beetle PCE / PC-FX / Saturn, Opera, Flycast,
//! FinalBurn Neo, SAME CDi, PUAE, PCSX2, PPSSPP (DVD CHD with 2 KiB hunks). RVZ: Dolphin.
//! No CHD: xemu (ISO only), Xenia, RPCS3, Cemu, Ryujinx/Eden, Virtual Jaguar.

use crate::jobs::LocalState;
use crate::library::has_ext;
use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Chdman,
    DolphinTool,
}

impl Tool {
    pub fn ext(self) -> &'static str {
        match self {
            Tool::Chdman => "chd",
            Tool::DolphinTool => "rvz",
        }
    }

    pub fn format(self) -> &'static str {
        match self {
            Tool::Chdman => "CHD",
            Tool::DolphinTool => "RVZ",
        }
    }

    fn bin(self) -> &'static str {
        match self {
            Tool::Chdman => "chdman",
            Tool::DolphinTool => "dolphin-tool",
        }
    }

    /// The RetroDECK component that ships it.
    fn component(self) -> &'static str {
        match self {
            Tool::Chdman => "mame",
            Tool::DolphinTool => "dolphin",
        }
    }
}

/// CD systems: `chdman createcd` from a .cue / .gdi / .iso.
const CD_SYSTEMS: &[&str] =
    &["psx", "segacd", "megacd", "pcenginecd", "pcfx", "3do", "saturn", "dreamcast", "neogeocd", "neogeocdjp", "cdimono1", "amigacd32"];
/// DVD systems: `chdman createdvd` from an .iso (a CD-based game's .cue still uses createcd).
const DVD_SYSTEMS: &[&str] = &["ps2", "psp"];
const RVZ_SYSTEMS: &[&str] = &["gc", "wii"];

/// The tool that packs this system's games, if its emulator reads the result.
pub fn target(system: &str) -> Option<Tool> {
    if CD_SYSTEMS.contains(&system) || DVD_SYSTEMS.contains(&system) {
        Some(Tool::Chdman)
    } else if RVZ_SYSTEMS.contains(&system) {
        Some(Tool::DolphinTool)
    } else {
        None
    }
}

/// The tool for this system, when it's installed.
pub fn usable(system: &str) -> Option<Tool> {
    target(system).filter(|&t| available(t))
}

/// Whether `rel` ("system/name.ext") is a disc image the tool turns into one file.
/// A .cue / .gdi brings its tracks along (see `crate::library::sheet_refs`).
pub fn is_input(rel: &str) -> bool {
    let system = rel.split('/').next().unwrap_or("");
    match target(system) {
        Some(Tool::Chdman) if DVD_SYSTEMS.contains(&system) => ["iso", "cue"].iter().any(|e| has_ext(rel, e)),
        Some(Tool::Chdman) => ["cue", "gdi", "iso"].iter().any(|e| has_ext(rel, e)),
        // .ciso is left alone: Dolphin plays it, it already drops the empty blocks, and
        // dolphin-tool 2512 asserts converting some (NFL Street 2, 2026-11).
        Some(Tool::DolphinTool) => ["iso", "gcm", "wbfs", "gcz"].iter().any(|e| has_ext(rel, e)),
        None => false,
    }
}

/// How to start a tool.
#[derive(Debug, Clone)]
enum Runner {
    Native(PathBuf),
    /// Inside RetroDECK's flatpak, at this path.
    RetroDeck(String),
}

const RETRODECK: &str = "net.retrodeck.retrodeck";

fn runner(tool: Tool) -> Option<&'static Runner> {
    static CHDMAN: OnceLock<Option<Runner>> = OnceLock::new();
    static DOLPHIN: OnceLock<Option<Runner>> = OnceLock::new();
    let cell = match tool {
        Tool::Chdman => &CHDMAN,
        Tool::DolphinTool => &DOLPHIN,
    };
    cell.get_or_init(|| find(tool)).as_ref()
}

pub fn available(tool: Tool) -> bool {
    runner(tool).is_some()
}

fn find(tool: Tool) -> Option<Runner> {
    let on_path = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join(tool.bin())).find(|p| p.is_file()))
        .map(Runner::Native);
    on_path.or_else(|| {
        let out = std::process::Command::new("flatpak")
            .args(["info", "--show-location", RETRODECK])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let loc = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let inside = format!("/app/retrodeck/components/{}/bin/{}", tool.component(), tool.bin());
        let on_host = Path::new(&loc).join("files").join(inside.trim_start_matches("/app/"));
        (out.status.success() && on_host.is_file()).then_some(Runner::RetroDeck(inside))
    })
}

/// Sets the component's library path the way its own `component_launcher.sh` does
/// (the line names runtime versions that change between RetroDECK releases), then runs
/// the tool: `sh -c SCRIPT sh <component> <tool> args...`.
const RETRODECK_SH: &str = r#"rd_shared_libs=/app/retrodeck/components/shared-libs
DEFAULT_LD_LIBRARY_PATH="$LD_LIBRARY_PATH"
line=$(grep -m1 '^export LD_LIBRARY_PATH=' "/app/retrodeck/components/$1/component_launcher.sh")
[ -n "$line" ] && eval "$line" || export LD_LIBRARY_PATH="$rd_shared_libs:$LD_LIBRARY_PATH"
shift
exec "$@""#;

fn command(tool: Tool, args: Vec<OsString>, dirs: &[&Path]) -> Result<tokio::process::Command> {
    let runner = runner(tool).ok_or_else(|| anyhow!("{} isn't installed (it comes with RetroDECK)", tool.bin()))?;
    let mut cmd = match runner {
        Runner::Native(p) => {
            let mut c = tokio::process::Command::new(p);
            c.args(args);
            c
        }
        Runner::RetroDeck(inside) => {
            let mut c = tokio::process::Command::new("flatpak");
            c.arg("run").arg("--command=sh");
            for d in dirs {
                let d = d.canonicalize().unwrap_or_else(|_| d.to_path_buf());
                c.arg(format!("--filesystem={}", d.display()));
            }
            c.arg(RETRODECK).args(["-c", RETRODECK_SH, "sh", tool.component(), inside]).args(args);
            c
        }
    };
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    Ok(cmd)
}

fn args(tool: Tool, system: &str, input: &Path, output: &Path) -> Vec<OsString> {
    let mut a: Vec<OsString> = match tool {
        Tool::Chdman if DVD_SYSTEMS.contains(&system) && has_ext(&input.to_string_lossy(), "iso") => {
            let mut a: Vec<OsString> = vec!["createdvd".into()];
            if system == "psp" {
                a.extend(["-hs".into(), "2048".into()]); // PPSSPP reads 2 KiB hunks
            }
            a
        }
        Tool::Chdman => vec!["createcd".into()],
        Tool::DolphinTool => ["convert", "-f", "rvz", "-b", "131072", "-c", "zstd", "-l", "5"].into_iter().map(Into::into).collect(),
    };
    a.extend(["-i".into(), input.as_os_str().to_owned(), "-o".into(), output.as_os_str().to_owned()]);
    if tool == Tool::Chdman {
        a.push("-f".into()); // overwrite a leftover .part
    }
    a
}

/// The first "NN.N%" in a progress line ("Compressing, 41.2% complete... (ratio=52.0%)").
fn percent(line: &str) -> Option<f64> {
    let end = line.find('%')?;
    let start = line[..end].rfind(|c: char| !(c.is_ascii_digit() || c == '.')).map_or(0, |i| i + 1);
    line[start..end].parse().ok()
}

/// One game packs at a time: chdman uses every core, and unzipping several discs at
/// once would just thrash the disk (and need room for all of them).
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Wait for this game's turn to unpack / pack; hold the guard while doing it.
pub async fn turn(what: &str, send: &impl Fn(LocalState)) -> tokio::sync::MutexGuard<'static, ()> {
    if let Ok(g) = LOCK.try_lock() {
        return g;
    }
    send(LocalState::Downloading { file: format!("waiting to pack {what} (one game at a time)"), done: 0, total: 0 });
    LOCK.lock().await
}

/// Pack the disc image `input` (with its tracks) into `dest_dir/<stem>.<chd|rvz>`.
/// `system` picks the tool and the CD / DVD flavour. Leaves the input alone; returns the
/// new file's path. The caller holds a `turn`.
pub async fn convert(system: &str, input: &Path, dest_dir: &Path, send: &impl Fn(LocalState)) -> Result<PathBuf> {
    let tool = usable(system).ok_or_else(|| anyhow!("no tool to pack {system} games"))?;
    let name = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&name).to_string();
    let dest = dest_dir.join(format!("{stem}.{}", tool.ext()));
    // Hidden while it's written, so a rescan doesn't pick it up half done.
    let part = dest_dir.join(format!(".{stem}.part.{}", tool.ext()));
    let total = disc_size(input);
    let label = format!("packing {stem} as {}", tool.format());

    send(LocalState::Downloading { file: label.clone(), done: 0, total });
    let a = args(tool, system, input, &part);
    let dirs: Vec<&Path> = [input.parent(), Some(dest_dir)].into_iter().flatten().collect();
    let mut child = command(tool, a, &dirs)?.spawn().with_context(|| format!("starting {}", tool.bin()))?;
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let mut log = String::new();
    let (mut b1, mut b2) = (vec![0u8; 4096], vec![0u8; 4096]);
    let (mut out_open, mut err_open) = (true, true);
    while out_open || err_open {
        let chunk = tokio::select! {
            n = out.read(&mut b1), if out_open => match n? { 0 => { out_open = false; continue } n => String::from_utf8_lossy(&b1[..n]).into_owned() },
            n = err.read(&mut b2), if err_open => match n? { 0 => { err_open = false; continue } n => String::from_utf8_lossy(&b2[..n]).into_owned() },
            () = tokio::time::sleep(Duration::from_secs(1)) => {
                // dolphin-tool prints no progress: show the output growing instead
                if tool == Tool::DolphinTool {
                    let n = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                    send(LocalState::Downloading { file: format!("{label} (written so far)"), done: n, total: 0 });
                }
                continue;
            }
        };
        for line in chunk.split(['\r', '\n']) {
            if line.contains("complete")
                && let Some(p) = percent(line)
            {
                send(LocalState::Downloading { file: label.clone(), done: (total as f64 * p / 100.0) as u64, total });
            }
        }
        log.push_str(&chunk);
        if log.len() > 64 * 1024 {
            log.drain(..log.len() - 16 * 1024);
        }
    }
    let status = child.wait().await?;
    if !status.success() || std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) == 0 {
        let _ = std::fs::remove_file(&part);
        let last: Vec<&str> =
            log.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains("complete...") && !l.contains("/esp")).collect();
        let tail = last[last.len().saturating_sub(3)..].join(" · ");
        bail!("{} failed on {name} ({status}): {tail}", tool.bin());
    }
    std::fs::rename(&part, &dest).with_context(|| format!("renaming to {}", dest.display()))?;
    Ok(dest)
}

/// A disc image's files: the image itself plus, for a .cue / .gdi, the tracks it names
/// (those that exist, next to it).
pub fn disc_files(input: &Path) -> Vec<PathBuf> {
    let dir = input.parent().unwrap_or_else(|| Path::new("."));
    let name = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut files = vec![input.to_path_buf()];
    if has_ext(&name, "cue") || has_ext(&name, "gdi") {
        let text = std::fs::read_to_string(input).unwrap_or_default();
        for r in crate::library::sheet_refs(&name, &text) {
            let p = dir.join(r.rsplit(['/', '\\']).next().unwrap_or(&r));
            if p.is_file() && !files.contains(&p) {
                files.push(p);
            }
        }
    }
    files
}

fn disc_size(input: &Path) -> u64 {
    disc_files(input).iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_tools_and_inputs() {
        assert_eq!(target("dreamcast"), Some(Tool::Chdman));
        assert_eq!(target("ps2"), Some(Tool::Chdman));
        assert_eq!(target("wii"), Some(Tool::DolphinTool));
        assert_eq!(target("xbox"), None);
        assert_eq!(target("snes"), None);
        assert!(is_input("psx/Game (USA).cue"));
        assert!(is_input("dreamcast/Game (USA).GDI"));
        assert!(is_input("psp/Game (USA).iso"));
        assert!(!is_input("psx/Game (USA) (Track 1).bin"));
        assert!(!is_input("psx/Game (USA).chd"));
        assert!(!is_input("ps2/Game (USA).gdi"));
        assert!(is_input("gc/Game (USA).iso"));
        assert!(!is_input("gc/Game (USA).ciso"));
        assert!(!is_input("gc/Game (USA).rvz"));
        assert!(!is_input("xbox/Game (USA).iso"));
    }

    #[test]
    fn builds_arguments() {
        let s = |v: Vec<OsString>| v.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        assert_eq!(s(args(Tool::Chdman, "psx", Path::new("/r/psx/G.cue"), Path::new("/r/psx/.G.part.chd"))), "createcd -i /r/psx/G.cue -o /r/psx/.G.part.chd -f");
        assert_eq!(s(args(Tool::Chdman, "ps2", Path::new("/r/ps2/G.iso"), Path::new("o"))), "createdvd -i /r/ps2/G.iso -o o -f");
        assert_eq!(s(args(Tool::Chdman, "psp", Path::new("/t/G.iso"), Path::new("o"))), "createdvd -hs 2048 -i /t/G.iso -o o -f");
        assert_eq!(s(args(Tool::Chdman, "ps2", Path::new("/r/ps2/G.cue"), Path::new("o"))), "createcd -i /r/ps2/G.cue -o o -f");
        assert!(s(args(Tool::DolphinTool, "gc", Path::new("/r/gc/G.iso"), Path::new("o"))).starts_with("convert -f rvz"));
    }

    #[test]
    fn reads_progress() {
        assert_eq!(percent("Compressing, 41.2% complete... (ratio=52.0%)"), Some(41.2));
        assert_eq!(percent("Compressing, 100% complete"), Some(100.0));
        assert_eq!(percent("no number"), None);
    }

    #[test]
    fn finds_disc_files() {
        let dir = std::env::temp_dir().join(format!("romgetter-discfiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("G.cue"), "FILE \"G (Track 1).bin\" BINARY\nFILE \"G (Track 2).bin\" BINARY\n").unwrap();
        std::fs::write(dir.join("G (Track 1).bin"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("G (Track 2).bin"), [0u8; 5]).unwrap();
        assert_eq!(disc_files(&dir.join("G.cue")).len(), 3);
        assert_eq!(disc_size(&dir.join("G.cue")), 15 + std::fs::metadata(dir.join("G.cue")).unwrap().len());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
