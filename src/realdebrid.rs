//! Minimal Real-Debrid client. Unlike TorBox, Real-Debrid lets us pick which files of a
//! torrent to download, so one game out of a multi-TB Minerva folder works even when
//! nobody has it cached: we add the folder's magnet, select just that file, and RD
//! fetches it from peers. Each game gets its own RD torrent (selection is per torrent).

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};

/// Real-Debrid allows 250 requests a minute; stay well under it.
const MIN_GAP: Duration = Duration::from_millis(300);
/// Tries per request when Real-Debrid answers 429 (too many requests).
const TRIES_ON_429: u32 = 6;

const BASE: &str = "https://api.real-debrid.com/rest/1.0";

#[derive(Clone)]
pub struct RealDebrid {
    http: reqwest::Client,
    key: String,
    /// Earliest time the next request may go out (shared by every clone).
    next_slot: Arc<Mutex<Instant>>,
    /// One game set up at a time (each setup is several requests and some waiting).
    adds: Arc<Semaphore>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RdFile {
    pub id: u64,
    #[serde(default)]
    pub path: String,
    /// 1 if the file is selected for download.
    #[serde(default)]
    pub selected: u8,
}

impl RdFile {
    pub fn base_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RdInfo {
    /// magnet_error, magnet_conversion, waiting_files_selection, queued, downloading,
    /// downloaded, error, virus, compressing, uploading, dead
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub files: Vec<RdFile>,
    /// One hoster link per selected file, once downloaded.
    #[serde(default)]
    pub links: Vec<String>,
}

impl RdInfo {
    pub fn is_ready(&self) -> bool {
        self.status == "downloaded" && !self.links.is_empty()
    }
    pub fn is_failed(&self) -> bool {
        matches!(self.status.as_str(), "magnet_error" | "error" | "virus" | "dead")
    }
    #[cfg(test)]
    fn selected(&self) -> Vec<&RdFile> {
        self.files.iter().filter(|f| f.selected == 1).collect()
    }
}

/// One torrent in the account listing. With one file selected, `filename` is that file.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ListItem {
    pub id: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub seeders: Option<u64>,
    #[serde(default)]
    pub speed: Option<u64>,
    #[serde(default)]
    pub links: Vec<String>,
}

impl ListItem {
    pub fn is_ready(&self) -> bool {
        self.status == "downloaded" && !self.links.is_empty()
    }
    pub fn is_failed(&self) -> bool {
        matches!(self.status.as_str(), "magnet_error" | "error" | "virus" | "dead")
    }
}

impl RealDebrid {
    pub fn new(http: reqwest::Client, key: String) -> Self {
        Self { http, key, next_slot: Arc::new(Mutex::new(Instant::now())), adds: Arc::new(Semaphore::new(1)) }
    }

    /// Wait for this request's turn under the shared rate limit.
    async fn wait_turn(&self, extra: Duration) {
        let at = {
            let mut next = self.next_slot.lock().await;
            let at = (*next).max(Instant::now()) + extra;
            *next = at + MIN_GAP;
            at
        };
        tokio::time::sleep_until(at.into()).await;
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<String> {
        let mut backoff = Duration::ZERO;
        for attempt in 1..=TRIES_ON_429 {
            let req = req.try_clone().ok_or_else(|| anyhow!("Real-Debrid request can't be retried"))?;
            self.wait_turn(backoff).await;
            let resp = req.bearer_auth(&self.key).send().await?;
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < TRIES_ON_429 {
                let after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                backoff = after.unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt))); // 2, 4, 8, 16, 32 s
                continue;
            }
            return Self::check(status, resp.text().await?);
        }
        bail!("Real-Debrid kept answering 429 (too many requests)")
    }

    fn check(status: reqwest::StatusCode, text: String) -> Result<String> {
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| text.chars().take(200).collect());
            bail!("Real-Debrid HTTP {status}: {msg}");
        }
        Ok(text)
    }

    /// Who the key belongs to ("user · premium until 2027-01-01"): checks a key typed into the Config tab.
    pub async fn whoami(&self) -> Result<String> {
        let text = self.send(self.http.get(format!("{BASE}/user"))).await?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        let user = v.get("username").and_then(|u| u.as_str()).unwrap_or("?");
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
        let until = v.get("expiration").and_then(|e| e.as_str()).map(|e| e.get(..10).unwrap_or(e)).unwrap_or("");
        Ok(if kind == "premium" && !until.is_empty() { format!("{user} · premium until {until}") } else { format!("{user} · {kind}") })
    }

    pub async fn info(&self, id: &str) -> Result<RdInfo> {
        let text = self.send(self.http.get(format!("{BASE}/torrents/info/{id}"))).await?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Every torrent in the account, with status (used for polling). Pages of 100: asked
    /// for more per page, Real-Debrid leaves out every torrent's `links` (seen 2026-11:
    /// limit=2500 returns "downloaded" torrents with no links), so nothing looks ready.
    pub async fn list(&self) -> Result<Vec<ListItem>> {
        const PAGE: usize = 100;
        let mut out = Vec::new();
        for page in 1..=50 {
            let req = self.http.get(format!("{BASE}/torrents")).query(&[("limit", PAGE.to_string()), ("page", page.to_string())]);
            let text = self.send(req).await?;
            let items: Vec<ListItem> = if text.trim().is_empty() { Vec::new() } else { serde_json::from_str(&text)? };
            let n = items.len();
            out.extend(items);
            if n < PAGE {
                break;
            }
        }
        Ok(out)
    }

    /// Put one file (`pick`, by base name) of a torrent on Real-Debrid; returns its RD
    /// torrent id. Reuses an RD torrent of the same hash that already selected that file.
    pub async fn add_one_file(&self, magnet: &str, btih: &str, pick: &str) -> Result<String> {
        let _turn = self.adds.acquire().await?;
        let want = pick.to_lowercase();
        // A one-file selection lists under that file's name: no per-torrent lookups needed.
        if let Some(item) = self
            .list()
            .await?
            .into_iter()
            .find(|i| i.hash.eq_ignore_ascii_case(btih) && !i.is_failed() && i.filename.to_lowercase() == want)
        {
            return Ok(item.id);
        }
        let text = self.send(self.http.post(format!("{BASE}/torrents/addMagnet")).form(&[("magnet", magnet)])).await?;
        let id = serde_json::from_str::<serde_json::Value>(&text)?
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Real-Debrid addMagnet: no id in {text}"))?;
        // Don't leave a half-set-up torrent (no file selected) behind on failure.
        let res = self.select_one(&id, pick).await;
        if res.is_err() {
            let _ = self.send(self.http.delete(format!("{BASE}/torrents/delete/{id}"))).await;
        }
        res.map(|()| id)
    }

    /// Put a whole torrent on Real-Debrid (every file selected) so it's cached there; returns
    /// its RD torrent id. Reuses an RD torrent of the same hash that already has every file.
    pub async fn add_all(&self, magnet: &str, btih: &str) -> Result<String> {
        let _turn = self.adds.acquire().await?;
        for item in self.list().await?.into_iter().filter(|i| i.hash.eq_ignore_ascii_case(btih) && !i.is_failed()) {
            if self.info(&item.id).await.is_ok_and(|info| !info.files.is_empty() && info.files.iter().all(|f| f.selected == 1)) {
                return Ok(item.id);
            }
        }
        let text = self.send(self.http.post(format!("{BASE}/torrents/addMagnet")).form(&[("magnet", magnet)])).await?;
        let id = serde_json::from_str::<serde_json::Value>(&text)?
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Real-Debrid addMagnet: no id in {text}"))?;
        let res = async {
            let start = Instant::now();
            loop {
                let info = self.info(&id).await?;
                match info.status.as_str() {
                    "waiting_files_selection" => break,
                    s if info.is_failed() => bail!("Real-Debrid couldn't read the torrent ({s})"),
                    "magnet_conversion" if start.elapsed() < Duration::from_secs(120) => tokio::time::sleep(Duration::from_secs(3)).await,
                    "magnet_conversion" => bail!("Real-Debrid is still fetching the torrent's file list; try again later"),
                    _ => return Ok(()), // already selected (cached single-file torrent)
                }
            }
            self.send(self.http.post(format!("{BASE}/torrents/selectFiles/{id}")).form(&[("files", "all")]))
                .await
                .context("selecting the files on Real-Debrid")
                .map(drop)
        }
        .await;
        if res.is_err() {
            let _ = self.send(self.http.delete(format!("{BASE}/torrents/delete/{id}"))).await;
        }
        res.map(|()| id)
    }

    async fn select_one(&self, id: &str, pick: &str) -> Result<()> {
        let want = pick.to_lowercase();
        // RD needs the torrent's metadata (from peers) before files can be picked.
        let start = Instant::now();
        let info = loop {
            let info = self.info(id).await?;
            match info.status.as_str() {
                "waiting_files_selection" => break info,
                s if info.is_failed() => bail!("Real-Debrid couldn't read the torrent ({s})"),
                "magnet_conversion" if start.elapsed() < Duration::from_secs(120) => tokio::time::sleep(Duration::from_secs(3)).await,
                "magnet_conversion" => bail!("Real-Debrid is still fetching the torrent's file list; try again later"),
                _ => break info, // already selected (e.g. a cached single-file torrent)
            }
        };
        if info.status == "waiting_files_selection" {
            let file = info
                .files
                .iter()
                .find(|f| f.base_name().to_lowercase() == want)
                .ok_or_else(|| anyhow!("'{pick}' isn't in this torrent on Real-Debrid ({} files)", info.files.len()))?;
            self.send(self.http.post(format!("{BASE}/torrents/selectFiles/{id}")).form(&[("files", file.id.to_string())]))
                .await
                .context("selecting the file on Real-Debrid")?;
        }
        Ok(())
    }

    /// A direct download URL for a finished RD torrent's (single selected) file.
    pub async fn download_url(&self, id: &str) -> Result<String> {
        let info = self.info(id).await?;
        if !info.is_ready() {
            bail!("Real-Debrid hasn't finished yet ({} {:.0}%)", info.status, info.progress);
        }
        Ok(self.unrestrict(&info.links[0]).await?.0)
    }

    /// A direct download for a hoster link (MEGA, Google Drive, …) or one of RD's own
    /// torrent links: (URL, file name).
    pub async fn unrestrict(&self, link: &str) -> Result<(String, String)> {
        let text = self.send(self.http.post(format!("{BASE}/unrestrict/link")).form(&[("link", link)])).await?;
        let v = serde_json::from_str::<serde_json::Value>(&text)?;
        let url = v
            .get("download")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Real-Debrid unrestrict: no download link in {text}"))?;
        let name = v
            .get("filename")
            .and_then(|v| v.as_str())
            .map_or_else(|| url.rsplit('/').next().unwrap_or("download").to_string(), str::to_string);
        Ok((url, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "network + Real-Debrid key from the config"]
    async fn rd_add_one_file_live() {
        let paths = crate::config::Paths::new().unwrap();
        let key = crate::config::Config::load(&paths.config_file).unwrap().rd_key().expect("real-debrid key");
        let rd = RealDebrid::new(reqwest::Client::new(), key);
        let h = "626a5e05f498e435145c97352aa2fecf7e95a668";
        let id = rd.add_one_file(&crate::minerva::magnet(h), h, "Armored Core 4 (USA).zip").await.unwrap();
        let info = rd.info(&id).await.unwrap();
        println!("{id}: {} {:.1}%", info.status, info.progress);
        assert_eq!(info.selected().len(), 1);
    }

    #[tokio::test]
    #[ignore = "network + Real-Debrid key from the config"]
    async fn rd_list_live() {
        let paths = crate::config::Paths::new().unwrap();
        let key = crate::config::Config::load(&paths.config_file).unwrap().rd_key().expect("real-debrid key");
        let rd = RealDebrid::new(reqwest::Client::new(), key);
        let items = rd.list().await.unwrap();
        for i in items.iter().filter(|i| i.status == "downloaded").take(8) {
            println!("{} {} ready={} links={}", i.id, i.filename, i.is_ready(), i.links.len());
        }
        println!("{} items", items.len());
    }

    #[test]
    fn parses_info() {
        let info: RdInfo = serde_json::from_str(
            r#"{"id":"QOF","hash":"626a","status":"downloading","progress":12.5,"seeders":3,"speed":1000,
                "files":[{"id":171,"path":"/Redump/Sony - PlayStation 3/Armored Core 4 (USA).zip","bytes":15351340062,"selected":1},
                         {"id":172,"path":"/Redump/Sony - PlayStation 3/Other.zip","bytes":1,"selected":0}],
                "links":[]}"#,
        )
        .unwrap();
        assert!(!info.is_ready() && !info.is_failed());
        assert_eq!(info.selected().len(), 1);
        assert_eq!(info.selected()[0].base_name(), "Armored Core 4 (USA).zip");
    }

    #[tokio::test]
    #[ignore = "network; needs the key in the config"]
    async fn whoami_live() {
        let paths = crate::config::Paths::new().unwrap();
        let key = crate::config::Config::load(&paths.config_file).unwrap().rd_key().expect("key");
        let who = RealDebrid::new(reqwest::Client::new(), key).whoami().await.unwrap();
        println!("{who}");
        assert!(!who.starts_with('?'), "{who}");
    }
}
