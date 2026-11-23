//! The IF Archive (ifarchive.org): free interactive fiction, played in RetroDECK by
//! Gargoyle. The archive publishes one big XML index of every file, with a description
//! that starts "Title, by Author."; that's indexed as a single set. Only story files
//! Gargoyle can open are kept (not zips, web builds or DOS executables). Downloads come
//! straight from ifarchive.org (free, small files), never through TorBox.

use crate::archive::{ItemIndex, RomFile};
use anyhow::{Context, Result};

/// Set key in `App::sets` and the index cache.
pub const KEY: &str = "ifarchive:games";
const INDEX_URL: &str = "https://ifarchive.org/indexes/Master-Index.xml";
const FILE_BASE: &str = "https://ifarchive.org/";
const GAMES: &str = "if-archive/games/";

/// Story-file extensions Gargoyle opens (RetroDECK's `zmachine` and `if` systems).
/// `.hex` (Hugo) and `.dat` are left out: elsewhere they're Arduboy ROMs and data files.
const STORY_EXT: &[&str] = &[
    "z1", "z2", "z3", "z4", "z5", "z6", "z7", "z8", "zblorb", "zlb", // Z-machine
    "ulx", "gblorb", "glb", "blb", // Glulx / Blorb
    "gam", "t3",  // TADS 2 / 3
    "taf", // ADRIFT
    "acd", "a3c", // Alan
    "agx", // AGT
    "l9", "mag", "jacl", "j2",
];

/// Folders whose files aren't the games to play: superseded releases and source code.
const SKIP_DIRS: &[&str] = &["old", "source"];

/// Whether a URL is an IF Archive file (downloaded directly, not through TorBox).
pub fn is_url(url: &str) -> bool {
    url.starts_with(FILE_BASE)
}

fn tag<'a>(chunk: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = chunk.split_once(&open)?;
    rest.split_once(&close).map(|(v, _)| v)
}

/// XML entities, including numeric ones (`&#34;`, `&#x27;`). `&amp;` goes last so
/// `&amp;lt;` stays `&lt;`.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("&#") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let decoded = after.split_once(';').and_then(|(num, tail)| {
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            Some((char::from_u32(code)?, tail))
        });
        if let Some((c, tail)) = decoded {
            out.push(c);
            rest = tail;
        } else {
            out.push_str("&#");
            rest = after;
        }
    }
    out.push_str(rest);
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// A line ending in one of these continues on the next ("Keeping Dido, written by\nJane Doe").
const CONTINUES: &[&str] = &["by", "and", "of", "the", "a", "an", "for", "in", "to", "with", "from", "par", "von", "de"];

fn is_story(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    let ext = base.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    STORY_EXT.contains(&ext.as_str())
}

/// "Photopia, by Adam Cadre. Version 1.30 (2012.0416)." → "Photopia, by Adam Cadre".
/// The first sentence of the description, skipping initials ("R. Sand"). A line break
/// ends it too, unless the line stops mid-phrase (on a comma or a word like "by").
pub fn title_from_description(desc: &str) -> Option<String> {
    let text = unescape(desc.trim());
    let mut para = String::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            break;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(line);
        let last = line.rsplit(' ').next().unwrap_or("").to_lowercase();
        if !(line.ends_with(',') || CONTINUES.contains(&last.as_str())) {
            break;
        }
    }
    let para = para.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = para.chars().collect();
    let mut end = chars.len();
    for i in 0..chars.len() {
        if chars[i] == '.' && chars.get(i + 1).is_none_or(|c| *c == ' ') {
            // "R." in "Ampe R. Sand" is an initial, not the end of the sentence.
            let initial = i >= 1 && chars[i - 1].is_uppercase() && (i == 1 || chars[i - 2] == ' ');
            if !initial {
                end = i;
                break;
            }
        }
    }
    let mut title: String = chars[..end].iter().collect();
    if title.chars().count() > 90 {
        title = title.chars().take(90).collect::<String>().trim_end().to_string() + "…";
    }
    let title = title.trim().trim_end_matches(',').trim().to_string();
    (!title.is_empty()).then_some(title)
}

/// Every story file under `if-archive/games/`, main format folders first (so their copy
/// wins over the same file in a competition folder).
pub fn parse_index(xml: &str) -> Vec<RomFile> {
    let mut out: Vec<(bool, RomFile)> = Vec::new();
    for chunk in xml.split("<file>").skip(1) {
        let chunk = chunk.split("</file>").next().unwrap_or(chunk);
        let Some(path) = tag(chunk, "path").map(unescape) else { continue };
        let Some(rel) = path.strip_prefix(GAMES) else { continue };
        if !is_story(rel) {
            continue;
        }
        let dirs: Vec<&str> = rel.split('/').collect();
        if dirs[..dirs.len() - 1].iter().any(|d| SKIP_DIRS.contains(&d.to_ascii_lowercase().as_str())) {
            continue;
        }
        let size = tag(chunk, "size").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        let url = format!("{FILE_BASE}{}", path.split('/').map(|s| urlencoding::encode(s).into_owned()).collect::<Vec<_>>().join("/"));
        let title = tag(chunk, "description").and_then(title_from_description);
        // games/zcode/x.z5 is a main release; competition and language folders go after.
        let main = dirs.len() == 2;
        out.push((main, RomFile { name: rel.to_string(), size, url: Some(url), torrent_index: None, title, system: None }));
    }
    out.sort_by(|(am, a), (bm, b)| bm.cmp(am).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out.into_iter().map(|(_, f)| f).collect()
}

/// Download and parse the archive's master index.
pub async fn fetch_index(http: &reqwest::Client) -> Result<ItemIndex> {
    let xml = http.get(INDEX_URL).send().await?.error_for_status()?.text().await.context("reading the IF Archive index")?;
    let files = parse_index(&xml);
    anyhow::ensure!(files.len() > 500, "IF Archive index looks wrong ({} story files)", files.len());
    let fetched = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok(ItemIndex {
        restricted: false,
        files,
        system: Some("if".into()),
        title: "IF Archive · interactive fiction".into(),
        fetched,
        btih: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems;

    const XML: &str = r#"<?xml version="1.0"?>
<ifarchive>
<file>
<name>photopia.z5</name>
<directory>if-archive/games/zcode</directory>
<path>if-archive/games/zcode/photopia.z5</path>
<size>239616</size>
<metadata>
  <item><key>tuid</key>
    <value>ju778uv5xaswnlpl</value>
  </item>
</metadata>
<description>

Photopia, by Adam Cadre. Version 1.30 (2012.0416).
(a DOS executable is in &lt;/if-archive/games/pc/#photopia.exe&gt;)
</description>
</file>
<file>
<name>photopia.z5</name>
<path>if-archive/games/competition98/inform/photopia/photopia.z5</path>
<size>200000</size>
</file>
<file>
<name>Counterfeit Monkey.gblorb</name>
<path>if-archive/games/glulx/Counterfeit Monkey.gblorb</path>
<size>9000000</size>
<description>Counterfeit Monkey, by Emily Short.
Release 7.</description>
</file>
<file>
<name>photo-r1.z5</name>
<path>if-archive/games/zcode/old/photo-r1.z5</path>
<size>1</size>
</file>
<file>
<name>game.zip</name>
<path>if-archive/games/zcode/game.zip</path>
<size>1</size>
</file>
<file>
<name>README</name>
<path>if-archive/README</path>
<size>1</size>
</file>
</ifarchive>"#;

    #[test]
    fn parses_story_files() {
        let files = parse_index(XML);
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["glulx/Counterfeit Monkey.gblorb", "zcode/photopia.z5", "competition98/inform/photopia/photopia.z5"]);
        assert_eq!(files[0].url.as_deref(), Some("https://ifarchive.org/if-archive/games/glulx/Counterfeit%20Monkey.gblorb"));
        assert_eq!(files[0].title.as_deref(), Some("Counterfeit Monkey, by Emily Short"));
        assert_eq!(files[1].title.as_deref(), Some("Photopia, by Adam Cadre"));
        assert_eq!(files[1].size, 239616);
        assert_eq!(files[2].title, None);
        assert_eq!(systems::guess_for_file(&files[1].name), Some("zmachine"));
        assert_eq!(systems::guess_for_file(&files[0].name), Some("if"));
    }

    #[test]
    fn titles() {
        assert_eq!(
            title_from_description("Ombre, une histoire courte par \"Ampe R. Sand\".").as_deref(),
            Some("Ombre, une histoire courte par \"Ampe R. Sand\"")
        );
        assert_eq!(
            title_from_description("Keeping Dido, a brief interactive story, written by\nJane Doe.\n\nMore.").as_deref(),
            Some("Keeping Dido, a brief interactive story, written by Jane Doe")
        );
        assert_eq!(
            title_from_description("The Water Bird, release 1.1, by Athan Skelley.").as_deref(),
            Some("The Water Bird, release 1.1, by Athan Skelley")
        );
        // a line break ends the title unless the line stops mid-phrase
        assert_eq!(
            title_from_description("City of Secrets, by Emily Short\nRelease 3 / Serial number 030624").as_deref(),
            Some("City of Secrets, by Emily Short")
        );
        assert_eq!(
            title_from_description("Fotopia, translated by &#34;Zak McKracken&#34; &amp;amp; friends.").as_deref(),
            Some("Fotopia, translated by \"Zak McKracken\" &amp; friends")
        );
        assert_eq!(title_from_description("   "), None);
    }

    /// `cargo test -- --ignored ifarchive_live`
    #[tokio::test]
    #[ignore = "network"]
    async fn ifarchive_live() {
        let http = reqwest::Client::builder().user_agent("romgetter-test").build().unwrap();
        let idx = fetch_index(&http).await.unwrap();
        let p = idx.files.iter().find(|f| f.name == "zcode/photopia.z5").unwrap();
        assert_eq!(p.title.as_deref(), Some("Photopia, by Adam Cadre"));
        let resp = http.get(p.url.as_deref().unwrap()).send().await.unwrap();
        assert!(resp.status().is_success(), "{}", resp.status());
        assert_eq!(resp.bytes().await.unwrap().len() as u64, p.size);
    }
}
