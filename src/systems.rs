//! RetroDECK (ES-DE) system folder names and heuristics for detecting them.
//!
//! Detection layers, strongest first:
//! - a file's own extension when it's unambiguous (`.sfc` → snes)
//! - folder names in a file's path (`Nintendo - Game Boy Advance/...`)
//! - the set's title / identifier / subject tags (word-boundary match, then a
//!   squashed substring match for CamelCase/underscore identifiers)
//! - a majority vote over the extensions of the files in a set

/// (folder name under `retrodeck/roms`, human label)
pub const SYSTEMS: &[(&str, &str)] = &[
    ("nes", "Nintendo - NES / Famicom"),
    ("fds", "Nintendo - Famicom Disk System"),
    ("snes", "Nintendo - SNES / Super Famicom"),
    ("satellaview", "Nintendo - Satellaview"),
    ("sufami", "Nintendo - Sufami Turbo"),
    ("n64", "Nintendo - 64"),
    ("gc", "Nintendo - GameCube"),
    ("wii", "Nintendo - Wii"),
    ("wiiu", "Nintendo - Wii U"),
    ("switch", "Nintendo - Switch"),
    ("gb", "Nintendo - Game Boy"),
    ("gbc", "Nintendo - Game Boy Color"),
    ("gba", "Nintendo - Game Boy Advance"),
    ("nds", "Nintendo - DS"),
    ("n3ds", "Nintendo - 3DS"),
    ("virtualboy", "Nintendo - Virtual Boy"),
    ("pokemini", "Nintendo - Pokemon Mini"),
    ("psx", "Sony - PlayStation"),
    ("ps2", "Sony - PlayStation 2"),
    ("ps3", "Sony - PlayStation 3"),
    ("psp", "Sony - PSP"),
    ("psvita", "Sony - PS Vita"),
    ("xbox", "Microsoft - Xbox"),
    ("xbox360", "Microsoft - Xbox 360"),
    ("sg-1000", "Sega - SG-1000"),
    ("mastersystem", "Sega - Master System"),
    ("gamegear", "Sega - Game Gear"),
    ("megadrive", "Sega - Mega Drive"),
    ("genesis", "Sega - Genesis"),
    ("segacd", "Sega - CD"),
    ("sega32x", "Sega - 32X"),
    ("saturn", "Sega - Saturn"),
    ("dreamcast", "Sega - Dreamcast"),
    ("naomi", "Sega - NAOMI"),
    ("atari2600", "Atari - 2600"),
    ("atari5200", "Atari - 5200"),
    ("atari7800", "Atari - 7800"),
    ("atarilynx", "Atari - Lynx"),
    ("atarijaguar", "Atari - Jaguar"),
    ("atarijaguarcd", "Atari - Jaguar CD"),
    ("pcengine", "NEC - PC Engine"),
    ("pcenginecd", "NEC - PC Engine CD"),
    ("tg16", "NEC - TurboGrafx-16"),
    ("pcfx", "NEC - PC-FX"),
    ("ngp", "SNK - Neo Geo Pocket"),
    ("ngpc", "SNK - Neo Geo Pocket Color"),
    ("neogeo", "SNK - Neo Geo"),
    ("neogeocd", "SNK - Neo Geo CD"),
    ("wonderswan", "Bandai - WonderSwan"),
    ("wonderswancolor", "Bandai - WonderSwan Color"),
    ("3do", "Panasonic - 3DO"),
    ("cdimono1", "Philips - CD-i"),
    ("intellivision", "Mattel - Intellivision"),
    ("colecovision", "Coleco - ColecoVision"),
    ("odyssey2", "Magnavox - Odyssey 2"),
    ("vectrex", "GCE - Vectrex"),
    ("msx", "Microsoft - MSX"),
    ("msx2", "Microsoft - MSX2"),
    ("amstradcpc", "Amstrad - CPC"),
    ("zxspectrum", "Sinclair - ZX Spectrum"),
    ("x68000", "Sharp - X68000"),
    ("c64", "Commodore - 64"),
    ("amiga", "Commodore - Amiga"),
    ("dos", "PC - MS-DOS"),
    ("zmachine", "Interactive Fiction - Z-machine"),
    ("if", "Interactive Fiction"),
    ("mame", "Arcade - MAME"),
    ("arcade", "Arcade"),
];

/// Ordered so that more specific names win ("game boy advance" before "game boy").
/// Patterns are written normalized: lowercase, words separated by single spaces.
const TITLE_PATTERNS: &[(&str, &str)] = &[
    ("game boy advance", "gba"),
    ("gameboy advance", "gba"),
    ("gba", "gba"),
    ("game boy color", "gbc"),
    ("gameboy color", "gbc"),
    ("gbc", "gbc"),
    ("game boy", "gb"),
    ("gameboy", "gb"),
    ("3ds", "n3ds"),
    ("nintendo ds", "nds"),
    ("nintendo dsi", "nds"),
    ("nds", "nds"),
    ("nintendo 64", "n64"),
    ("n64", "n64"),
    ("satellaview", "satellaview"),
    ("sufami turbo", "sufami"),
    ("super nintendo", "snes"),
    ("super famicom", "snes"),
    ("snes", "snes"),
    ("sfc", "snes"),
    ("famicom disk", "fds"),
    ("family computer disk", "fds"),
    ("fds", "fds"),
    ("nintendo entertainment system", "nes"),
    ("family computer", "nes"),
    ("famicom", "nes"),
    ("nes", "nes"),
    ("gamecube", "gc"),
    ("game cube", "gc"),
    ("gcn", "gc"),
    ("wii u", "wiiu"),
    ("wiiu", "wiiu"),
    ("wii", "wii"),
    ("nintendo switch", "switch"),
    ("switch", "switch"),
    ("virtual boy", "virtualboy"),
    ("pokemon mini", "pokemini"),
    ("playstation portable", "psp"),
    ("psp", "psp"),
    ("playstation vita", "psvita"),
    ("ps vita", "psvita"),
    ("psvita", "psvita"),
    ("playstation 3", "ps3"),
    ("ps3", "ps3"),
    ("playstation 2", "ps2"),
    ("ps2", "ps2"),
    ("playstation", "psx"),
    ("psx", "psx"),
    ("ps1", "psx"),
    ("xbox 360", "xbox360"),
    ("xbox360", "xbox360"),
    ("xbox", "xbox"),
    ("sg 1000", "sg-1000"),
    ("sg1000", "sg-1000"),
    ("master system", "mastersystem"),
    ("mark iii", "mastersystem"),
    ("sms", "mastersystem"),
    ("game gear", "gamegear"),
    ("gamegear", "gamegear"),
    ("mega cd", "segacd"),
    ("megacd", "segacd"),
    ("sega cd", "segacd"),
    ("segacd", "segacd"),
    ("32x", "sega32x"),
    ("mega drive", "megadrive"),
    ("megadrive", "megadrive"),
    ("smd", "megadrive"),
    ("genesis", "genesis"),
    ("saturn", "saturn"),
    ("dreamcast", "dreamcast"),
    ("naomi", "naomi"),
    ("atari 2600", "atari2600"),
    ("vcs", "atari2600"),
    ("atari 5200", "atari5200"),
    ("atari 7800", "atari7800"),
    ("lynx", "atarilynx"),
    ("jaguar cd", "atarijaguarcd"),
    ("jaguar", "atarijaguar"),
    ("pc engine cd", "pcenginecd"),
    ("pc engine super cd", "pcenginecd"),
    ("turbografx cd", "pcenginecd"),
    ("turbografx 16 cd", "pcenginecd"),
    ("pc engine", "pcengine"),
    ("pce", "pcengine"),
    ("turbografx", "tg16"),
    ("tg16", "tg16"),
    ("pc fx", "pcfx"),
    ("pcfx", "pcfx"),
    ("neo geo pocket color", "ngpc"),
    ("neo geo pocket", "ngp"),
    ("ngpc", "ngpc"),
    ("ngp", "ngp"),
    ("neo geo cd", "neogeocd"),
    ("neogeo cd", "neogeocd"),
    ("neo geo", "neogeo"),
    ("neogeo", "neogeo"),
    ("wonderswan color", "wonderswancolor"),
    ("wonderswan", "wonderswan"),
    ("3do", "3do"),
    ("philips cd i", "cdimono1"),
    ("cd i", "cdimono1"),
    ("cdi", "cdimono1"),
    ("intellivision", "intellivision"),
    ("colecovision", "colecovision"),
    ("odyssey 2", "odyssey2"),
    ("odyssey2", "odyssey2"),
    ("videopac", "odyssey2"),
    ("vectrex", "vectrex"),
    ("msx2", "msx2"),
    ("msx", "msx"),
    ("amstrad cpc", "amstradcpc"),
    ("amstrad", "amstradcpc"),
    ("zx spectrum", "zxspectrum"),
    ("x68000", "x68000"),
    ("commodore 64", "c64"),
    ("c64", "c64"),
    ("amiga", "amiga"),
    ("exodos", "dos"),
    ("ms dos", "dos"),
    ("msdos", "dos"),
    ("total dos", "dos"),
    ("dos collection", "dos"),
    ("dos games", "dos"),
    ("z machine", "zmachine"),
    ("zmachine", "zmachine"),
    ("zcode", "zmachine"),
    ("infocom", "zmachine"),
    ("interactive fiction", "if"),
    ("text adventure", "if"),
    ("finalburn", "arcade"),
    ("fbneo", "arcade"),
    ("mame", "mame"),
    ("arcade", "arcade"),
];

/// Short squashed patterns that are still safe to match as raw substrings
/// (e.g. inside "SonyPSXISORedumpSet"). Anything 6+ chars is considered safe.
const SAFE_SHORT: &[&str] = &["psx", "snes", "n64", "gba", "gbc", "3ds", "ps2", "ps3", "psp", "wiiu", "nesfiles", "msdos"];

/// Extensions that identify a system on their own. Ambiguous ones
/// (.bin .iso .chd .cue .zip .7z .rvz ...) are deliberately left out.
const EXTENSIONS: &[(&str, &str)] = &[
    ("nes", "nes"),
    ("unf", "nes"),
    ("fds", "fds"),
    ("sfc", "snes"),
    ("smc", "snes"),
    ("bs", "satellaview"),
    ("st", "sufami"),
    ("z64", "n64"),
    ("n64", "n64"),
    ("v64", "n64"),
    ("ndd", "n64"),
    ("gb", "gb"),
    ("gbc", "gbc"),
    ("gba", "gba"),
    ("nds", "nds"),
    ("dsi", "nds"),
    ("3ds", "n3ds"),
    ("cia", "n3ds"),
    ("cci", "n3ds"),
    ("cxi", "n3ds"),
    ("gcm", "gc"),
    ("gcz", "gc"),
    ("wbfs", "wii"),
    ("wad", "wii"),
    ("wua", "wiiu"),
    ("wux", "wiiu"),
    ("rpx", "wiiu"),
    ("nsp", "switch"),
    ("xci", "switch"),
    ("nsz", "switch"),
    ("xcz", "switch"),
    ("vb", "virtualboy"),
    ("min", "pokemini"),
    ("pbp", "psx"),
    ("cso", "psp"),
    ("vpk", "psvita"),
    ("sg", "sg-1000"),
    ("sms", "mastersystem"),
    ("gg", "gamegear"),
    ("md", "megadrive"),
    ("gen", "megadrive"),
    ("smd", "megadrive"),
    ("32x", "sega32x"),
    ("gdi", "dreamcast"),
    ("cdi", "dreamcast"),
    ("a26", "atari2600"),
    ("a52", "atari5200"),
    ("a78", "atari7800"),
    ("lnx", "atarilynx"),
    ("j64", "atarijaguar"),
    ("jag", "atarijaguar"),
    ("pce", "pcengine"),
    ("ngp", "ngp"),
    ("ngc", "ngpc"),
    ("ws", "wonderswan"),
    ("wsc", "wonderswancolor"),
    ("int", "intellivision"),
    ("col", "colecovision"),
    ("o2", "odyssey2"),
    ("vec", "vectrex"),
    ("d64", "c64"),
    ("t64", "c64"),
    ("prg", "c64"),
    ("adf", "amiga"),
    ("tzx", "zxspectrum"),
    ("z80", "zxspectrum"),
    ("cdt", "amstradcpc"),
    ("dsk", "amstradcpc"),
    ("dim", "x68000"),
    // interactive fiction story files (Gargoyle). "z80" above is a Spectrum snapshot.
    ("z1", "zmachine"),
    ("z2", "zmachine"),
    ("z3", "zmachine"),
    ("z4", "zmachine"),
    ("z5", "zmachine"),
    ("z6", "zmachine"),
    ("z7", "zmachine"),
    ("z8", "zmachine"),
    ("zblorb", "zmachine"),
    ("zlb", "zmachine"),
    ("ulx", "if"),
    ("gblorb", "if"),
    ("glb", "if"),
    ("blb", "if"),
    ("t3", "if"),
    ("taf", "if"),
    ("a3c", "if"),
];

/// Lowercase, turn every non-alphanumeric into a single space, and pad with spaces so
/// whole-word matching is `contains(" pat ")`. Contains both the plain form and a
/// CamelCase-split form ("PlayStation" stays matchable, "SonyPSXSet" splits).
fn normalize(s: &str) -> String {
    format!("{} | {}", normalize_one(s, false), normalize_one(s, true))
}

fn normalize_one(s: &str, split_camel: bool) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    out.push(' ');
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if let Some(p) = prev
                && split_camel
                && p.is_lowercase()
                && c.is_uppercase()
            {
                out.push(' ');
            }
            out.extend(c.to_lowercase());
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
        prev = Some(c);
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Platforms RetroDECK doesn't emulate. A title naming one of these is *not* a match
/// for a nearby supported system ("PlayStation 5" must not become psx).
const UNSUPPORTED: &[&str] = &[
    "playstation 4",
    "playstation 5",
    "ps4",
    "ps5",
    "xbox one",
    "xbox series",
    "series x",
    "windows phone",
    "ios",
    "ipa",
    "iphone",
    "ipad",
    "android",
    "apk",
    "xapk",
    "j2me",
    "java me",
    "n gage",
    "ngage",
    "nokia",
    "symbian",
    "stadia",
    "steam",
    "windows pc",
    "macos",
];

pub fn is_unsupported_platform(title: &str) -> bool {
    let hay = normalize(title);
    UNSUPPORTED.iter().any(|p| hay.contains(&format!(" {p} ")))
}

/// Guess a system folder from free text (a set title, identifier, folder name, ...).
pub fn guess_from_title(title: &str) -> Option<&'static str> {
    if is_unsupported_platform(title) {
        return None;
    }
    let hay = normalize(title);
    if let Some((_, sys)) = TITLE_PATTERNS.iter().find(|(pat, _)| hay.contains(&format!(" {pat} "))) {
        return Some(sys);
    }
    // Second pass for identifiers like "SNES-No-Intro_2024" or "SonyPSXISORedumpSet".
    let squashed = squash(title);
    TITLE_PATTERNS
        .iter()
        .find(|(pat, _)| {
            let p = squash(pat);
            (p.len() >= 6 || SAFE_SHORT.contains(&p.as_str())) && squashed.contains(&p)
        })
        .map(|(_, sys)| *sys)
}

/// Exact system name/alias for a search phrase: "ps3", "snes", "game boy advance",
/// "megadrive", a folder name like "n3ds", or a label like "sony - playstation 3".
pub fn alias(phrase: &str) -> Option<&'static str> {
    // Every system's names, normalized once: search parses thousands of names a keystroke.
    static NAMES: std::sync::OnceLock<Vec<(String, &'static str)>> = std::sync::OnceLock::new();
    let p = normalize_one(phrase, false);
    let p = p.trim();
    if p.is_empty() {
        return None;
    }
    let names = NAMES.get_or_init(|| {
        let mut v = Vec::new();
        for (f, l) in SYSTEMS {
            // "if" is an ordinary word in titles ("What If"): only "interactive fiction" names it.
            if *f != "if" {
                v.push((normalize_one(f, false).trim().to_string(), *f));
            }
            v.push((normalize_one(l, false).trim().to_string(), *f));
        }
        v.extend(TITLE_PATTERNS.iter().map(|(pat, sys)| (pat.to_string(), *sys)));
        v
    });
    names.iter().find(|(n, _)| n == p).map(|(_, f)| *f)
}

/// A parsed search: words that name a system act as system filters
/// ("zelda gba", "god of war ps3", "mario game boy advance"); the rest match text.
#[derive(Debug, Default, PartialEq)]
pub struct Query {
    pub text: Vec<String>,
    /// (the words as typed, the system they name)
    pub systems: Vec<(String, &'static str)>,
}

impl Query {
    pub fn parse(q: &str) -> Self {
        let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
        let mut out = Query::default();
        let mut i = 0;
        'outer: while i < words.len() {
            // longest system name starting here (up to 4 words)
            for len in (1..=4.min(words.len() - i)).rev() {
                let phrase = words[i..i + len].join(" ");
                if let Some(sys) = alias(&phrase) {
                    out.systems.push((phrase, sys));
                    i += len;
                    continue 'outer;
                }
            }
            out.text.push(words[i].clone());
            i += 1;
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.systems.is_empty()
    }

    /// `text`: lowercase name + set/item title; `system`: the hit's system folder.
    /// Text words match at the start of a word ("sonic" ≠ "Supersonic"). A system word
    /// matches the hit's system; only hits with no known system fall back to the text.
    pub fn matches(&self, text: &str, system: &str) -> bool {
        self.matches_any(&[text], system)
    }

    /// Like `matches`, with the text in parts (e.g. the file's name, the set's title):
    /// each word may match in any part. Allocation-free, for searching every file.
    pub fn matches_any(&self, texts: &[&str], system: &str) -> bool {
        let label = label_lower(system);
        self.text.iter().all(|t| texts.iter().any(|x| starts_word(x, t)) || starts_word(label, t))
            && self.systems.iter().all(|(phrase, sys)| {
                if system.is_empty() { texts.iter().any(|x| contains_word(x, phrase)) } else { same_system(sys, system) }
            })
    }

    /// `matches_any` for the files of one set, the set's text checked once: words it
    /// already has needn't match each file.
    pub fn for_set(&self, set_text: &str) -> SetQuery<'_> {
        SetQuery {
            text: self.text.iter().map(String::as_str).filter(|t| !starts_word(set_text, t)).collect(),
            systems: self.systems.iter().map(|(phrase, sys)| (phrase.as_str(), *sys, contains_word(set_text, phrase))).collect(),
        }
    }

    /// Words to send to a title search (system words are applied as filters afterwards).
    pub fn text_query(&self) -> String {
        self.text.join(" ")
    }
}

/// A `Query` ready for one set's files (see `Query::for_set`).
pub struct SetQuery<'a> {
    /// Text words the set's text doesn't have.
    text: Vec<&'a str>,
    /// (system words, the system, the set's text has them)
    systems: Vec<(&'a str, &'static str, bool)>,
}

impl SetQuery<'_> {
    /// Same as `Query::matches_any(&[name, set_text], system)`; `label` is `label_lower(system)`.
    pub fn matches(&self, name: &str, label: &str, system: &str) -> bool {
        self.text.iter().all(|t| starts_word(name, t) || starts_word(label, t))
            && self.systems.iter().all(
                |&(phrase, sys, in_set)| {
                    if system.is_empty() { in_set || contains_word(name, phrase) } else { same_system(sys, system) }
                },
            )
    }
}

/// Folders that are the same hardware under regional names.
const SAME_HARDWARE: &[&[&str]] = &[&["megadrive", "genesis"], &["pcengine", "tg16"], &["mame", "arcade"]];

pub fn same_system(a: &str, b: &str) -> bool {
    a == b || SAME_HARDWARE.iter().any(|g| g.contains(&a) && g.contains(&b))
}

/// `word` occurs at the start of a word in `hay`.
fn starts_word(hay: &str, word: &str) -> bool {
    hay.match_indices(word).any(|(i, _)| hay[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()))
}

fn contains_word(hay: &str, word: &str) -> bool {
    hay.match_indices(word).any(|(i, _)| {
        let before = hay[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = hay[i + word.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        before && after
    })
}

/// Guess a system folder from a ROM file's extension.
pub fn guess_from_filename(name: &str) -> Option<&'static str> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let (_, ext) = base.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    EXTENSIONS.iter().find(|(e, _)| *e == ext).map(|(_, s)| *s)
}

/// Guess from the folders of a path, deepest folder first.
pub fn guess_from_dirs(path: &str) -> Option<&'static str> {
    let (dirs, _) = path.rsplit_once('/')?;
    dirs.rsplit('/').find_map(|seg| {
        // "SNES.zip" as a path segment means "inside SNES.zip": drop the extension
        let seg = seg.strip_suffix(".zip").or_else(|| seg.strip_suffix(".7z")).unwrap_or(seg);
        guess_from_title(seg)
    })
}

/// Best guess for one file: unambiguous extension, then its folders.
pub fn guess_for_file(path: &str) -> Option<&'static str> {
    guess_from_filename(path).or_else(|| guess_from_dirs(path))
}

/// Majority vote over file extensions; needs 60% agreement among guessable files.
pub fn vote<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut total = 0;
    for n in names {
        if let Some(s) = guess_from_filename(n) {
            total += 1;
            match counts.iter_mut().find(|(k, _)| *k == s) {
                Some((_, c)) => *c += 1,
                None => counts.push((s, 1)),
            }
        }
    }
    let (sys, n) = counts.into_iter().max_by_key(|(_, c)| *c)?;
    (n * 10 >= total * 6).then_some(sys)
}

/// Guess from a pasted magnet or URL: its display name / file name.
pub fn guess_from_link(link: &str) -> Option<&'static str> {
    let name = if let Some(q) = link.strip_prefix("magnet:?") {
        q.split('&').find_map(|kv| kv.strip_prefix("dn=")).map(|v| v.replace('+', " "))?
    } else {
        let path = link.split(['?', '#']).next().unwrap_or(link);
        path.trim_end_matches('/').rsplit('/').next()?.to_string()
    };
    let name = urlencoding::decode(&name).map(std::borrow::Cow::into_owned).unwrap_or(name);
    guess_from_filename(&name).or_else(|| guess_from_title(&name))
}

/// `label`, lowercase (cached).
pub fn label_lower(folder: &str) -> &'static str {
    static LOWER: std::sync::OnceLock<std::collections::HashMap<&'static str, String>> = std::sync::OnceLock::new();
    let map = LOWER.get_or_init(|| SYSTEMS.iter().map(|(f, l)| (*f, l.to_lowercase())).collect());
    map.get(folder).map(String::as_str).unwrap_or("unknown")
}

/// The static name of a system folder ("" if unknown).
pub fn folder_static(folder: &str) -> &'static str {
    SYSTEMS.iter().find(|(f, _)| *f == folder).map(|(f, _)| *f).unwrap_or("")
}

pub fn label(folder: &str) -> &'static str {
    SYSTEMS.iter().find(|(f, _)| *f == folder).map(|(_, l)| *l).unwrap_or("unknown")
}

/// Step through SYSTEMS from `current` by `delta` (wrapping). Unknown/empty starts at the ends.
pub fn cycle(current: &str, delta: isize) -> &'static str {
    let n = SYSTEMS.len() as isize;
    let next = match SYSTEMS.iter().position(|(f, _)| *f == current) {
        Some(i) => (i as isize + delta).rem_euclid(n),
        None if delta >= 0 => 0,
        None => n - 1,
    };
    SYSTEMS[next as usize].0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_guesses() {
        let cases = [
            ("[No-Intro] Sega - Master System - Mark III", Some("mastersystem")),
            ("SNES No-Intro 2024 Myrient Collection", Some("snes")),
            ("Nintendo - Gameboy No-Intro", Some("gb")),
            ("Nintendo - Game Boy Advance", Some("gba")),
            ("Sega Genesis set", Some("genesis")),
            ("Nintendo 3DS", Some("n3ds")),
            ("(No-Intro) Wii U CDN", Some("wiiu")),
            ("Random stuff", None),
            // No-Intro / Redump DAT names
            ("Nintendo - Nintendo Entertainment System (Headered)", Some("nes")),
            ("Nintendo - Family Computer Disk System (FDS)", Some("fds")),
            ("Nintendo - Super Nintendo Entertainment System", Some("snes")),
            ("Sega - Mega Drive - Genesis", Some("megadrive")),
            ("Sega - Mega-CD - Sega CD", Some("segacd")),
            ("NEC - PC Engine - TurboGrafx-16", Some("pcengine")),
            ("NEC - PC Engine CD & TurboGrafx CD", Some("pcenginecd")),
            ("SNK - Neo Geo Pocket Color", Some("ngpc")),
            ("Sony - PlayStation Portable", Some("psp")),
            ("Sony - PlayStation 2", Some("ps2")),
            ("Atari - Jaguar CD", Some("atarijaguarcd")),
            ("Microsoft - Xbox 360", Some("xbox360")),
            ("Sega - SG-1000", Some("sg-1000")),
            // identifiers
            ("SonyPSXISORedumpSet_2020-01-28_01", Some("psx")),
            ("Redump_PSX_2021_06_04_A_C", Some("psx")),
            ("No-Intro_SNES_Collection", Some("snes")),
            ("nointro.ms-mkiii", None),
            ("ef_Nintendo_Gameboy_No-Intro_2024-04-23", Some("gb")),
            ("PS3_NOINTRO_USA_8", Some("ps3")),
            ("NintendoGameCubeRedump", Some("gc")),
            ("eXoDOSRPG", Some("dos")),
            ("eXoDOS", Some("dos")),
            ("[CDDL] [Антология] Total DOS Collection #24 (Blood, Carmageddon, Doom)", Some("dos")),
            ("MS-DOS Games", Some("dos")),
            ("Dos Hermanos (2010)", None),
            ("Super Monkey Ball (GCN)", Some("gc")),
            ("Super Monkey Ball Banana Mania (PlayStation 5)", None),
            ("Super Monkey Ball Banana Mania (Xbox Series X)", None),
            ("Super Monkey Ball 1 IOS", None),
            ("Super Monkey Ball: Tip 'n Tilt (Java ME / J2ME Mobile Game)", None),
            ("Super Monkey Ball Adventure (PSP)", Some("psp")),
            ("Super Monkey Ball Deluxe (Original Xbox)", Some("xbox")),
            ("Super Monkey Ball 2 (GameCube)", Some("gc")),
            ("Super Monkey Ball: Touch & Roll (Nintendo DS)", Some("nds")),
            // must not false-positive
            ("Phoenix Genesis Remaster", Some("genesis")),
            ("Business Software Collection", None),
            ("Game Pack 4 GB", None),
        ];
        for (t, want) in cases {
            assert_eq!(guess_from_title(t), want, "{t}");
        }
    }

    #[test]
    fn file_guesses() {
        assert_eq!(guess_from_filename("Super Mario World (USA).sfc"), Some("snes"));
        assert_eq!(guess_from_filename("game.7z"), None);
        assert_eq!(guess_from_filename("Game (USA).bin"), None);
        assert_eq!(guess_for_file("SNES.zip/SNES/Super Mario World (USA).zip"), Some("snes"));
        assert_eq!(guess_for_file("Nintendo - Game Boy Advance/Metroid Fusion (USA).zip"), Some("gba"));
        assert_eq!(guess_for_file("roms/Metroid Fusion (USA).gba"), Some("gba"));
        assert_eq!(guess_for_file("Metroid Fusion (USA).zip"), None);
    }

    #[test]
    fn link_guesses() {
        assert_eq!(guess_from_link("https://host/dl/Chrono%20Trigger%20(USA).sfc?token=1"), Some("snes"));
        assert_eq!(guess_from_link("magnet:?xt=urn:btih:abc&dn=Sony+-+PlayStation+2+Redump&tr=x"), Some("ps2"));
        assert_eq!(guess_from_link("https://host/file/abc123"), None);
    }

    #[test]
    fn query_parsing() {
        let q = Query::parse("God of War PS3");
        assert_eq!(q.text, vec!["god", "of", "war"]);
        assert_eq!(q.systems, vec![("ps3".to_string(), "ps3")]);
        let q = Query::parse("metroid game boy advance");
        assert_eq!(q.text, vec!["metroid"]);
        assert_eq!(q.systems[0].1, "gba");
        let q = Query::parse("sonic mega drive");
        assert_eq!(q.systems[0].1, "megadrive");
        assert!(Query::parse("sonic genesis").matches("sonic the hedgehog (usa, europe).zip", "megadrive"));
        let q = Query::parse("mario kart");
        assert_eq!(q.systems, [] as [(String, &str); 0]);
        assert_eq!(Query::parse("ps1 crash").systems[0].1, "psx");
        assert_eq!(Query::parse("n3ds").systems[0].1, "n3ds");
        // interactive fiction: "zork infocom" filters, but "if" stays a plain word
        assert_eq!(Query::parse("zork infocom").systems[0].1, "zmachine");
        assert_eq!(Query::parse("photopia interactive fiction").systems[0].1, "if");
        let q = Query::parse("what if");
        assert_eq!(q.text, vec!["what", "if"]);
        assert_eq!(q.systems, [] as [(String, &str); 0]);
        assert_eq!(guess_from_filename("photopia.z5"), Some("zmachine"));
        assert_eq!(guess_from_filename("Counterfeit Monkey.gblorb"), Some("if"));
        assert_eq!(guess_from_filename("Manic Miner.z80"), Some("zxspectrum"));
    }

    #[test]
    fn query_matching() {
        let q = Query::parse("zelda snes");
        assert!(q.matches("legend of zelda, the - a link to the past (usa).zip", "snes"));
        assert!(!q.matches("legend of zelda, the (usa).zip", "nes"));
        // "nes" must not match inside "snes" or "jones"
        let q = Query::parse("nes");
        assert!(!q.matches("indiana jones (usa).zip", "snes"));
        assert!(q.matches("whatever (nes).zip", "")); // unknown system: the name decides
        // a known system wins over the name ("wii" ≠ Wii U, NES re-release on GBA ≠ nes)
        assert!(!Query::parse("mario kart wii").matches("mario kart 8 mario-kart-8-wii-u", "wiiu"));
        assert!(!Query::parse("metroid nes").matches("classic nes series - metroid (usa, europe).zip", "gba"));
        // word starts only
        assert!(!Query::parse("sonic").matches("aerobiz supersonic (usa).7z", "megadrive"));
        assert!(Query::parse("bandic").matches("crash bandicoot (usa).zip", "psx"));
        // label words: "sony" matches PlayStation systems
        let q = Query::parse("sony crash");
        assert!(q.matches("crash bandicoot (usa).chd", "psx"));
        // set title / region text
        let q = Query::parse("redump europe");
        assert!(q.matches("game (europe).zip ps3 redump 2024 individual games (part 1)", "ps3"));
    }

    #[test]
    fn voting() {
        assert_eq!(vote(["a.gba", "b.gba", "c.gba", "d.txt", "e.zip"]), Some("gba"));
        assert_eq!(vote(["a.gba", "b.nds"]), None);
        assert_eq!(vote(["a.zip", "b.zip"]), None);
    }

    #[test]
    fn cycling_wraps() {
        assert_eq!(cycle("nes", -1), SYSTEMS[SYSTEMS.len() - 1].0);
        assert_eq!(cycle("nes", 1), "fds");
        assert_eq!(cycle("", 1), "nes");
        assert_eq!(cycle("", -1), SYSTEMS[SYSTEMS.len() - 1].0);
    }
}
