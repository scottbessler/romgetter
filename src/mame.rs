//! MAME ROM zips are named by MAME's short names ("sf2.zip"), so search can't find
//! "street fighter" from the file name alone. This loads libretro's MAME list (short
//! name → full title) and caches it as JSON; search and the Library use the titles.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

const DAT_URL: &str = "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/mame/MAME.dat";
const CACHE_FILE: &str = "mame-titles.json";
const REFRESH: Duration = Duration::from_hours(720);

/// Systems whose ROMs are MAME short-name zips.
pub fn is_mame_system(system: &str) -> bool {
    matches!(system, "mame" | "arcade")
}

/// "sf2.zip" → "sf2"
pub fn short_name(file: &str) -> Option<String> {
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = base.strip_suffix(".zip").or_else(|| base.strip_suffix(".7z"))?;
    (!stem.is_empty() && !stem.contains(' ')).then(|| stem.to_lowercase())
}

/// Parse a clrmamepro dat: `game ( name "Title" ... rom ( name short.zip ... ) )`.
pub fn parse_dat(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut title: Option<String> = None;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("game (") {
            title = None;
        } else if let Some(rest) = l.strip_prefix("name \"") {
            title = rest.strip_suffix('"').map(str::to_string);
        } else if let Some(rest) = l.strip_prefix("rom ( name ") {
            let file = rest.split_whitespace().next().unwrap_or("");
            if let (Some(t), Some(short)) = (title.as_ref(), short_name(file)) {
                out.entry(short).or_insert_with(|| t.clone());
            }
        }
    }
    out
}

/// The title map: from the cache when it's fresh, otherwise downloaded (falling back to
/// a stale cache if the download fails).
pub async fn load(http: &reqwest::Client, cache_dir: &Path) -> Result<HashMap<String, String>> {
    let path = cache_dir.join(CACHE_FILE);
    let cached = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<HashMap<String, String>>(&b).ok());
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < REFRESH);
    if let Some(map) = &cached
        && fresh
        && !map.is_empty()
    {
        return Ok(cached.unwrap());
    }
    let fetched: Result<HashMap<String, String>> = async {
        let text = http.get(DAT_URL).send().await?.error_for_status()?.text().await?;
        let map = parse_dat(&text);
        anyhow::ensure!(map.len() > 1000, "MAME list looks wrong ({} games)", map.len());
        Ok(map)
    }
    .await;
    match fetched {
        Ok(map) => {
            let _ = std::fs::create_dir_all(cache_dir);
            let _ = std::fs::write(&path, serde_json::to_vec(&map)?);
            Ok(map)
        }
        Err(e) => cached.filter(|m| !m.is_empty()).ok_or(e).context("loading MAME game titles"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mame_dat() {
        let dat = r#"
clrmamepro (
	name "MAME - Consolidated ROM Sets"
)

game (
	name "Street Fighter II: The World Warrior (World 910522)"
	year "1991"
	developer "Capcom"
	rom ( name sf2.zip size 2 crc 1 md5 2 sha1 3 )
)

game (
	name "10-Yard Fight (World, set 1)"
	rom ( name 10yard.zip size 62708 crc 8F401426 )
)
"#;
        let m = parse_dat(dat);
        assert_eq!(m.get("sf2").map(String::as_str), Some("Street Fighter II: The World Warrior (World 910522)"));
        assert_eq!(m.get("10yard").map(String::as_str), Some("10-Yard Fight (World, set 1)"));
        assert_eq!(m.len(), 2);
        assert_eq!(short_name("MAME 0.260 ROMs (non-merged)/sf2.zip").as_deref(), Some("sf2"));
        assert_eq!(short_name("Street Fighter II (USA).zip"), None);
    }
}
