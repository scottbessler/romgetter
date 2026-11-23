//! Game names across sources: what makes two files "the same game" in search, and which
//! copy of a game is best.
//!
//! Sources name files differently: No-Intro/Redump ("Legend of Zelda, The - Twilight
//! Princess (USA) (Rev 1).zip"), site-tagged sets ("The Legend of Zelda- Twilight
//! Princess (axekin.com).rvz") and scene-numbered DS sets ("5141 - Lets Play Flight
//! Attendant (U)(SUXXORS)(NDS6.com).7z"). A game's key keeps its title and the tags that
//! make it a different product (disc number, DLC, Virtual Console, translation, ...) and
//! drops region, language, version, beta/demo, site and release-group tags, so every
//! version of a game is one search row and the version is picked from its sources.

use std::cmp::Reverse;

/// The file name without folders or extension. Saves keep theirs: they aren't a copy of the game.
fn stem(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((_, e)) if ["sav", "srm"].contains(&e.to_ascii_lowercase().as_str()) => base,
        Some((s, e)) if !s.is_empty() && (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()) => s,
        _ => base,
    }
}

/// A scene release number in front of the title ("0502 - ", "3177 "). Years (1900–2099)
/// are left alone: they start real titles ("1942", "2010 FIFA World Cup").
fn strip_release_number(s: &str) -> (&str, bool) {
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits != 4 {
        return (s, false);
    }
    let n: u32 = s[..4].parse().unwrap_or(0);
    if (1900..2100).contains(&n) {
        return (s, false);
    }
    let rest = &s[4..];
    let after = rest.strip_prefix(" - ").or_else(|| rest.strip_prefix(' ').filter(|_| !(1000..2100).contains(&n)));
    match after {
        Some(a) if a.chars().next().is_some_and(|c| !c.is_whitespace()) => (a, true),
        _ => (s, false),
    }
}

/// Title and tags: "Game (USA) (Rev 1)[b]" → ("Game", ["USA", "Rev 1", "[b]"]).
fn split_tags(s: &str) -> (&str, Vec<String>) {
    let cut = s.find(['(', '[']).unwrap_or(s.len());
    let mut tags = Vec::new();
    let mut rest = &s[cut..];
    while let Some(start) = rest.find(['(', '[']) {
        let close = if rest[start..].starts_with('(') { ')' } else { ']' };
        let Some(end) = rest[start..].find(close) else { break };
        let inner = &rest[start + 1..start + end];
        tags.push(if close == ']' { format!("[{inner}]") } else { inner.trim().to_string() });
        rest = &rest[start + end + 1..];
    }
    (s[..cut].trim(), tags)
}

const REGIONS: &[&str] = &[
    "usa",
    "world",
    "europe",
    "japan",
    "asia",
    "australia",
    "brazil",
    "canada",
    "china",
    "france",
    "germany",
    "italy",
    "korea",
    "netherlands",
    "spain",
    "sweden",
    "united kingdom",
    "uk",
    "taiwan",
    "hong kong",
    "russia",
    "scandinavia",
    "poland",
    "portugal",
    "denmark",
    "finland",
    "norway",
    "greece",
    "india",
    "latin america",
    "south korea",
    "mexico",
    "argentina",
    "belgium",
    "austria",
    "switzerland",
    "ireland",
    "new zealand",
    "south africa",
    "turkey",
    "israel",
    "czech",
    "hungary",
    "croatia",
    "unknown",
    // TV standards, as uploads often name the region
    "ntsc",
    "ntsc-u",
    "ntsc-us",
    "ntsc-j",
    "ntsc-uc",
    "pal",
    "pal-e",
    "pal-eu",
    "secam",
];

/// Languages spelled out ("(English)", "(Eng)").
const LANGUAGES: &[&str] = &[
    "english",
    "eng",
    "french",
    "german",
    "spanish",
    "italian",
    "japanese",
    "portuguese",
    "dutch",
    "swedish",
    "russian",
    "korean",
    "chinese",
    "polish",
    "multi",
    "multilanguage",
    "multi-language",
];

/// Scene sets' short region codes → No-Intro names.
fn scene_region(tag: &str) -> Option<&'static str> {
    Some(match tag.to_ascii_lowercase().as_str() {
        "u" | "us" | "usa" => "USA",
        "e" | "eu" | "eur" => "Europe",
        "j" | "jp" | "jpn" => "Japan",
        "g" | "ger" => "Germany",
        "f" | "fr" => "France",
        "s" | "spa" => "Spain",
        "i" | "ita" => "Italy",
        "k" | "ks" | "kor" => "Korea",
        "uk" => "United Kingdom",
        "w" | "wld" => "World",
        _ => return None,
    })
}

fn is_region(tag: &str) -> bool {
    let t = tag.to_lowercase();
    scene_region(&t).is_some() || t.split(',').all(|p| REGIONS.contains(&p.trim()))
}

/// "(En,Fr,De)", "(Zh-Hans)", "(En-US)", "(English)", "(M5)" (5 languages).
fn is_languages(tag: &str) -> bool {
    let t = tag.to_lowercase();
    if t.strip_prefix('m').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
        return true;
    }
    tag.split(',').all(|p| {
        let p = p.trim();
        if LANGUAGES.contains(&p.to_lowercase().as_str()) {
            return true;
        }
        let (lang, script) = p.split_once('-').unwrap_or((p, ""));
        lang.len() == 2 && lang.chars().all(|c| c.is_ascii_alphabetic()) && script.len() <= 4
    })
}

fn is_version(tag: &str) -> bool {
    let t = tag.to_lowercase();
    t == "rev" || t.starts_with("rev ") || t.starts_with("version") || t == "alt" || t.starts_with("alt ") || {
        let v = t.strip_prefix('v').unwrap_or(&t);
        v.chars().next().is_some_and(|c| c.is_ascii_digit()) && v.chars().all(|c| c.is_ascii_digit() || c == '.')
    }
}

/// Betas, demos, dated builds and other not-quite-the-game versions.
fn is_prerelease(tag: &str) -> bool {
    const WORDS: &[&str] =
        &["beta", "proto", "demo", "sample", "trial", "kiosk", "debug", "taikenban", "preview", "promo", "possible proto"];
    let t = tag.to_lowercase();
    WORDS.iter().any(|w| t == *w || t.starts_with(&format!("{w} ")) || t.ends_with(&format!(" {w}")))
        || is_date(&t)
        // trade-show builds: "E3 2005", "Tokyo Game Show 2004"
        || t.starts_with("e3") || t.contains("game show") || t.starts_with("gamescom")
}

fn is_date(t: &str) -> bool {
    let d: String = t.chars().filter(char::is_ascii_digit).collect();
    (d.len() == 8 || d.len() == 6) && t.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// "(axekin.com)", "(NDS6.com)": the site that packed it.
fn is_site(tag: &str) -> bool {
    let t = tag.to_lowercase();
    [".com", ".net", ".org", ".info", ".ru", ".to", ".cc"].iter().any(|d| t.ends_with(d))
}

/// Bad/verified/alternate dump flags: "[b]", "[!]", "[a]".
fn is_dump_flag(tag: &str) -> bool {
    matches!(tag.to_lowercase().as_str(), "[b]" | "[!]" | "[a]" | "[o]" | "[h]" | "[f]" | "[p]") || tag.to_lowercase().starts_with("[b")
}

/// Tags that don't change which game it is (they pick a version of it).
fn is_variant(tag: &str, system: &str) -> bool {
    is_region(tag)
        || is_languages(tag)
        || is_version(tag)
        || is_prerelease(tag)
        || is_site(tag)
        || is_dump_flag(tag)
        || matches!(tag.to_lowercase().as_str(), "edc" | "unl" | "rom")
        || own_system(tag, system)
}

/// The tag names the game's own system ("(PSP)" on a PSP game). Memoized: the alias
/// lookup scans every system pattern, and search runs this for thousands of tags.
fn own_system(tag: &str, system: &str) -> bool {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static ALIASES: RefCell<HashMap<String, Option<&'static str>>> = RefCell::new(HashMap::new());
    }
    if tag.len() > 40 || tag.starts_with('[') {
        return false;
    }
    let found = ALIASES.with(|m| *m.borrow_mut().entry(tag.to_string()).or_insert_with(|| crate::systems::alias(tag)));
    found.is_some_and(|s| crate::systems::same_system(s, system))
}

/// Uploads that append the system to the title: "NFL Street. Nintendo Gamecube",
/// "nfl street 3. PSP", "Halo - Xbox". Only the game's own system is cut.
fn strip_system_suffix<'a>(title: &'a str, system: &str) -> &'a str {
    let mut t = title.trim();
    for _ in 0..2 {
        let cut = [". ", " - ", ", ", " – "].iter().filter_map(|sep| t.rfind(sep).map(|i| (i, sep.len()))).max();
        match cut {
            Some((i, n)) if i > 0 && own_system(t[i + n..].trim(), system) => t = t[..i].trim_end_matches(['.', ' ', '-']),
            _ => break,
        }
    }
    t
}

/// Kept for scene-numbered names, whose other tags are release groups and dump info.
fn is_product_tag(tag: &str) -> bool {
    let t = tag.to_lowercase();
    ["disc", "disk", "side", "tape", "dlc", "addon", "update", "theme"].iter().any(|w| t.starts_with(w))
}

/// A file name taken apart for comparing with other sources' copies.
#[derive(Debug, Clone, PartialEq)]
pub struct GameName {
    /// Title as written, without the scene number ("Legend of Zelda, The - Twilight Princess").
    pub title: String,
    /// Tags that make it a different product ("Disc 2", "DLC", "Virtual Console").
    pub product: Vec<String>,
}

pub fn parse(system: &str, name: &str) -> GameName {
    let s = stem(name).replace('_', " ");
    let (s, numbered) = strip_release_number(s.trim());
    let (title, tags) = split_tags(s);
    // scene names (release number, one-letter regions, packed ")(" tags): only discs etc. matter
    let scene = numbered || s.contains(")(") || tags.iter().any(|t| t.len() == 1 && scene_region(t).is_some());
    let mut product: Vec<String> = tags.into_iter().filter(|t| !is_variant(t, system) && (!scene || is_product_tag(t))).collect();
    if ["sav", "srm"].iter().any(|e| crate::library::has_ext(name, e)) {
        product.push("save".into());
    }
    let title = strip_system_suffix(title, system);
    GameName { title: title.split_whitespace().collect::<Vec<_>>().join(" "), product }
}

/// A title folded for matching: case, punctuation, "&"/"and" and articles don't count
/// ("The Legend of Zelda- Twilight Princess" = "Legend of Zelda, The - Twilight Princess").
fn fold(title: &str) -> String {
    let t = title.to_lowercase().replace('&', " and ").replace(['\'', '’', '`'], "");
    let t: String = t.chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    t.split_whitespace().filter(|w| !matches!(*w, "the" | "a" | "an")).collect::<Vec<_>>().join(" ")
}

impl GameName {
    pub fn key(&self, system: &str) -> String {
        let tags: Vec<String> = self.product.iter().map(|t| fold(t)).collect();
        let title = fold(&self.title);
        // a name that's all tags ("(Disc 1).bin") mustn't match every other such file
        let title = if title.is_empty() { format!("\u{1}{}", self.display()) } else { title };
        format!("{system}\0{title}\0{}", tags.join("\0"))
    }

    /// How a search row names the game ("Crash Bandicoot", "Final Fantasy VII (Disc 2)").
    pub fn display(&self) -> String {
        let mut s = if self.title.is_empty() { "?".to_string() } else { self.title.clone() };
        for t in &self.product {
            s.push(' ');
            if t.starts_with('[') {
                s.push_str(t);
            } else {
                s.push('(');
                s.push_str(t);
                s.push(')');
            }
        }
        s
    }
}

/// Sort key for a game's copies, best first: the finished game over betas/demos/bad
/// dumps, USA over World over Europe over the rest, then the latest revision.
pub fn preference(name: &str) -> (u8, u8, Reverse<(u32, String)>) {
    let s = stem(name).replace('_', " ");
    let (s, _) = strip_release_number(s.trim());
    let (_, tags) = split_tags(s);
    let lower: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
    let junk = u8::from(
        tags.iter().any(|t| is_prerelease(t) || is_dump_flag(t))
            || lower.iter().any(|t| matches!(t.as_str(), "unl" | "pirate" | "hack" | "aftermarket") || t.starts_with("[h")),
    );
    let regions: Vec<String> = tags
        .iter()
        .filter(|t| is_region(t))
        .flat_map(|t| match scene_region(t) {
            Some(r) => vec![r.to_lowercase()],
            None => t.to_lowercase().split(',').map(|p| p.trim().to_string()).collect(),
        })
        .collect();
    let region = if regions.iter().any(|r| r == "usa") {
        0
    } else if regions.iter().any(|r| r == "world") {
        1
    } else if regions.iter().any(|r| r == "europe" || r == "united kingdom" || r == "uk") {
        2
    } else if regions.is_empty() {
        3 // unknown region (site-tagged sets are usually USA)
    } else {
        4
    };
    // "Rev 2" > "Rev 1" > none; "Rev B" > "Rev A"; "v1.1" > "v1.0"
    let rev = tags
        .iter()
        .filter(|t| is_version(t))
        .map(|t| {
            let t = t.to_lowercase();
            let v = t.trim_start_matches("rev").trim_start_matches("version").trim().trim_start_matches('v').to_string();
            let n = v.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
            (n + 1, v)
        })
        .max()
        .unwrap_or((0, String::new()));
    (junk, region, Reverse(rev))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game_key(system: &str, name: &str) -> String {
        parse(system, name).key(system)
    }

    #[test]
    fn same_game_across_sources() {
        let k = |sys, n| game_key(sys, n);
        let zelda = k("gc", "Legend of Zelda, The - Twilight Princess (USA).zip");
        assert_eq!(zelda, k("gc", "The Legend of Zelda- Twilight Princess (axekin.com).rvz"));
        assert_eq!(zelda, k("gc", "Legend of Zelda, The - Twilight Princess (Europe) (En,Fr,De,Es,It) (Rev 1).7z"));
        assert_eq!(
            k("nds", "Let's Play Flight Attendant (USA).zip"),
            k("nds", "5141 - Lets Play Flight Attendant (U)(SUXXORS)(NDS6.com).7z")
        );
        assert_eq!(
            k("nds", "Mysterious Case of Dr. Jekyll and Mr. Hyde, The (Europe).zip"),
            k("nds", "5273 - Mysterious Case of Dr. Jekyll and Mr. Hyde The (E)(Independent)(NDS6.com).zip")
        );
        assert_eq!(k("psx", "crash_bandicoot_(usa).ZIP"), k("psx", "Crash Bandicoot (Japan) (Beta) (1996-05-02).zip"));
        assert_eq!(k("psp", "ModNation Racers (USA) (PSP) (PSN).zip"), k("psp", "ModNation Racers (Europe) (PSN).zip"));
        assert_eq!(k("nds", "Polarium (USA).zip"), k("nds", "0006 - Polarium (U)(Trashman).zip"));
        // uploads that append the system
        assert_eq!(k("gc", "NFL Street (USA).zip"), k("gc", "NFL Street. Nintendo Gamecube.iso"));
        assert_eq!(k("psp", "NFL Street 2 - Unleashed (USA).zip"), k("psp", "nfl street 2 unleashed. PSP.iso"));
        assert_eq!(k("psp", "NFL Street 3 (USA).zip"), k("psp", "nfl street 3. PSP.cso"));
        assert_eq!(k("xbox", "Halo (USA).zip"), k("xbox", "Halo - Xbox.iso"));
        assert_eq!(parse("gc", "NFL Street. Nintendo Gamecube.iso").title, "NFL Street");
        // a title that merely ends in another system's name keeps it
        assert_eq!(parse("gba", "Sonic Genesis (USA).zip").title, "Sonic Genesis");
        assert_eq!(k("gc", "Zelda (USA).zip"), k("gc", "Zelda (NTSC) (PT-BR).rar"));
        assert_eq!(k("gc", "Zelda (USA).zip"), k("gc", "Zelda (PAL) (English).rar"));
        assert_eq!(k("nds", "Polarium (USA).zip"), k("nds", "Polarium (USA) (Demo) (Kiosk) (E3 2005) [b].zip"));
        // different products stay apart
        assert_ne!(k("psx", "Final Fantasy VII (USA) (Disc 1).zip"), k("psx", "Final Fantasy VII (USA) (Disc 2).zip"));
        assert_ne!(k("wii", "Super Mario Bros. (USA) (NES) (Virtual Console).zip"), k("wii", "Super Mario Bros. (USA) (Wii).zip"));
        assert_ne!(k("ps3", "Game (USA) (DLC).zip"), k("ps3", "Game (USA).zip"));
        assert_ne!(k("gba", "Game (USA).zip"), k("gba", "Game (USA).sav"));
        // titles that start with numbers keep them
        assert_ne!(k("arcade", "1942 (Japan).zip"), k("arcade", "1943 (Japan).zip"));
        assert_eq!(parse("psp", "2010 FIFA World Cup - South Africa (Europe) (Alt).zip").title, "2010 FIFA World Cup - South Africa");
        assert_eq!(parse("nds", "3177 Momotarou Dentetsu - 20 Shuunen (JP).zip").title, "Momotarou Dentetsu - 20 Shuunen");
        assert_eq!(parse("psx", "Final Fantasy VII (USA) (Disc 2).zip").display(), "Final Fantasy VII (Disc 2)");
    }

    #[test]
    fn prefers_usa_latest_finished() {
        let mut names = vec![
            "Game (Japan).zip",
            "Game (USA) (Beta).zip",
            "Game (Europe).zip",
            "Game (USA).zip",
            "Game (USA) (Rev 2).zip",
            "Game (USA) (Rev 1).zip",
            "Game (USA) (Demo).zip",
            "Game (World).zip",
            "0123 - Game (U)(Group).zip",
        ];
        names.sort_by_key(|n| preference(n));
        assert_eq!(&names[..3], &["Game (USA) (Rev 2).zip", "Game (USA) (Rev 1).zip", "Game (USA).zip"]);
        assert_eq!(names[3], "0123 - Game (U)(Group).zip");
        assert_eq!(&names[4..7], &["Game (World).zip", "Game (Europe).zip", "Game (Japan).zip"]);
        assert!(names[7..].iter().all(|n| n.contains("Beta") || n.contains("Demo")));
    }
}
