//! Metacritic's best-rated games per platform (its public "finder" backend, the one the
//! website's browse pages use), and matching those titles to ROM file names.

use anyhow::{Context, Result};

const FINDER: &str = "https://backend.metacritic.com/finder/metacritic/web";

/// RetroDECK system → Metacritic platform id. Metacritic only covers the N64/PS1 era on.
const PLATFORMS: &[(&str, u64)] = &[
    ("n64", 1500000084),
    ("psx", 1500000078),
    ("dreamcast", 1500000067),
    ("ps2", 1500000094),
    ("gc", 1500000099),
    ("xbox", 1500000098),
    ("gba", 1500000091),
    ("nds", 1500000108),
    ("psp", 1500000109),
    ("wii", 1500000114),
    ("ps3", 1500000113),
    ("xbox360", 1500000111),
    ("n3ds", 1500000116),
    ("psvita", 1500000117),
    ("wiiu", 1500000118),
    ("switch", 1500000124),
];

pub fn platform(system: &str) -> Option<u64> {
    PLATFORMS.iter().find(|(s, _)| *s == system).map(|(_, id)| *id)
}

/// Metacritic has no rankings before the N64/PS1, so these systems get a fixed list of
/// the usual consensus classics (titles spelled to match No-Intro names).
const CLASSICS: &[(&str, &[&str])] = &[
    (
        "nes",
        &[
            "Super Mario Bros. 3",
            "The Legend of Zelda",
            "Super Mario Bros.",
            "Mega Man 2",
            "Metroid",
            "Zelda II: The Adventure of Link",
            "Castlevania III: Dracula's Curse",
            "Mike Tyson's Punch-Out!!",
            "Kirby's Adventure",
            "Contra",
            "Ninja Gaiden",
            "DuckTales",
            "Castlevania",
            "Final Fantasy",
            "Mega Man 3",
            "Super Mario Bros. 2",
            "Tecmo Super Bowl",
            "River City Ransom",
            "Blaster Master",
            "Dragon Warrior",
        ],
    ),
    (
        "snes",
        &[
            "Super Mario World",
            "The Legend of Zelda: A Link to the Past",
            "Super Metroid",
            "Chrono Trigger",
            "Final Fantasy III",
            "Super Mario Kart",
            "Donkey Kong Country",
            "Super Mario RPG: Legend of the Seven Stars",
            "EarthBound",
            "Super Mario World 2: Yoshi's Island",
            "Donkey Kong Country 2: Diddy's Kong Quest",
            "Street Fighter II Turbo",
            "Super Castlevania IV",
            "Mega Man X",
            "Secret of Mana",
            "Star Fox",
            "F-Zero",
            "Final Fantasy II",
            "Contra III: The Alien Wars",
            "Kirby Super Star",
        ],
    ),
    (
        "megadrive",
        &[
            "Sonic the Hedgehog 2",
            "Streets of Rage 2",
            "Gunstar Heroes",
            "Sonic the Hedgehog 3",
            "Sonic & Knuckles",
            "Sonic the Hedgehog",
            "Phantasy Star IV",
            "Shining Force II",
            "Castlevania: Bloodlines",
            "Contra: Hard Corps",
            "Ecco the Dolphin",
            "Earthworm Jim",
            "Mortal Kombat II",
            "Vectorman",
            "ToeJam & Earl",
            "Comix Zone",
            "Ristar",
            "Rocket Knight Adventures",
            "Streets of Rage 3",
            "Golden Axe",
        ],
    ),
];

/// Where a system's top list comes from.
pub enum TopList {
    Metacritic(u64),
    Classics(Vec<String>),
}

pub fn top_list(system: &str) -> Option<TopList> {
    if let Some((_, list)) = CLASSICS.iter().find(|(s, _)| *s == system) {
        return Some(TopList::Classics(list.iter().map(ToString::to_string).collect()));
    }
    platform(system).map(TopList::Metacritic)
}

/// The `n` best-reviewed games for a platform, by Metascore (titles without the year).
pub async fn top(http: &reqwest::Client, platform: u64, n: usize) -> Result<Vec<String>> {
    let resp = http
        .get(FINDER)
        .header(reqwest::header::USER_AGENT, "Mozilla/5.0 (X11; Linux x86_64) romgetter")
        .query(&[
            ("sortBy", "-metaScore".to_string()),
            ("productType", "games".into()),
            ("mcoTypeId", "13".into()),
            ("gamePlatformIds", platform.to_string()),
            ("limit", n.to_string()),
            ("offset", "0".into()),
        ])
        .send()
        .await?
        .error_for_status()?;
    let v: serde_json::Value = resp.json().await.context("reading Metacritic's list")?;
    let items = v.pointer("/data/items").and_then(|i| i.as_array()).context("Metacritic: unexpected response")?;
    Ok(items.iter().filter_map(|i| i.get("title")?.as_str()).map(strip_year).take(n).collect())
}

/// "The Legend of Zelda: Ocarina of Time (1998)" → "The Legend of Zelda: Ocarina of Time"
fn strip_year(t: &str) -> String {
    let t = t.trim();
    match t.rsplit_once(" (") {
        Some((head, tail)) if tail.len() == 5 && tail.ends_with(')') && tail[..4].chars().all(|c| c.is_ascii_digit()) => head.to_string(),
        _ => t.to_string(),
    }
}

/// A comparable form of a game title, from either a Metacritic title or a ROM file name
/// ("Legend of Zelda, The - Ocarina of Time (USA).zip"): no region/version tags, no
/// articles or punctuation, "&" as "and", lowercase.
pub fn norm(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    // ROM tags start at the first " (" or " ["; Metacritic titles rarely use brackets.
    let cut = [base.find(" ("), base.find(" [")].into_iter().flatten().min().unwrap_or(base.len());
    let mut s = base[..cut].to_lowercase();
    if let Some(stem) = s.strip_suffix(".zip").or_else(|| s.strip_suffix(".7z")).or_else(|| s.strip_suffix(".iso")) {
        s = stem.to_string();
    }
    let s = s.replace('&', " and ").replace(['\'', '’', '™', '®', '.'], "");
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !matches!(*w, "the" | "a" | "an")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_titles_to_rom_names() {
        assert_eq!(strip_year("The Legend of Zelda: Ocarina of Time (1998)"), "The Legend of Zelda: Ocarina of Time");
        assert_eq!(strip_year("Halo 3"), "Halo 3");
        let mc = norm(&strip_year("The Legend of Zelda: Ocarina of Time (1998)"));
        assert_eq!(mc, norm("Legend of Zelda, The - Ocarina of Time (USA) (Rev 2).zip"));
        assert_eq!(norm("Ratchet & Clank Future: Tools of Destruction"), norm("Ratchet and Clank Future - Tools of Destruction (USA).zip"));
        assert_eq!(norm("Uncharted 2: Among Thieves"), norm("Uncharted 2 - Among Thieves (USA) (En,Fr,De,Es,It).zip"));
        assert_eq!(norm("Tom Clancy's Splinter Cell"), norm("Tom Clancys Splinter Cell (USA).iso"));
        assert_ne!(norm("Halo 3"), norm("Halo 3 - ODST (USA).zip"));
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn metacritic_live() {
        let t = top(&reqwest::Client::new(), platform("ps3").unwrap(), 20).await.unwrap();
        println!("{t:#?}");
        assert_eq!(t.len(), 20);
    }
}
