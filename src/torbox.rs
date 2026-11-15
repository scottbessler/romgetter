//! Minimal TorBox API client: web downloads + torrents.
//!
//! Response shapes aren't formally documented, so parsing is lenient.

use crate::library::has_ext;
use anyhow::{Result, anyhow, bail};
use reqwest::multipart::Form;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const BASE: &str = "https://api.torbox.app/v1/api";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    WebDl,
    Torrent,
}

impl Kind {
    fn path(self) -> &'static str {
        match self {
            Kind::WebDl => "webdl",
            Kind::Torrent => "torrents",
        }
    }
    fn id_param(self) -> &'static str {
        match self {
            Kind::WebDl => "web_id",
            Kind::Torrent => "torrent_id",
        }
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    data: Value,
}

fn nd<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Item {
    #[serde(default, deserialize_with = "nd")]
    pub id: u64,
    #[serde(default, deserialize_with = "nd")]
    pub name: String,
    #[serde(default, deserialize_with = "nd")]
    pub hash: String,
    #[serde(default, deserialize_with = "nd")]
    pub size: f64,
    #[serde(default, deserialize_with = "nd")]
    pub progress: f64,
    #[serde(default, deserialize_with = "nd")]
    pub download_state: String,
    #[serde(default, deserialize_with = "nd")]
    pub download_finished: bool,
    #[serde(default, deserialize_with = "nd")]
    pub download_present: bool,
    /// TorBox packed the whole torrent into one zip (it does this for 100+ file torrents
    /// unless `allow_zip=false` was sent).
    #[serde(default, deserialize_with = "nd")]
    pub allow_zipped: bool,
    #[serde(default, deserialize_with = "nd")]
    pub files: Vec<RemoteFile>,
}

impl Item {
    pub fn is_zipped(&self) -> bool {
        self.files.len() == 1 && (self.files[0].zipped || self.allow_zipped)
    }

    /// Whether one game (`pick`, at torrent `index`) out of a big torrent is in this item.
    /// TorBox sometimes has only part of a torrent cached (e.g. the one file somebody
    /// else selected, plus a `.parts` file) and still reports it "cached". A zipped item
    /// can't be checked without reading the zip, so it counts as present.
    pub fn has_file(&self, pick: &str, index: Option<u32>) -> bool {
        let want = pick.to_lowercase();
        self.is_zipped()
            || self.files.iter().any(|f| f.file_name().to_lowercase() == want)
            || index.is_some_and(|i| !self.is_partial() && self.files.iter().any(|f| f.id == u64::from(i)))
    }

    /// Only part of the torrent is here; file ids then don't match torrent indices.
    pub fn is_partial(&self) -> bool {
        self.files.iter().any(|f| has_ext(&f.name, "parts"))
    }

    /// Real files, without TorBox's `.parts` placeholder for a partial torrent.
    pub fn real_files(&self) -> usize {
        self.files.iter().filter(|f| !has_ext(&f.name, "parts")).count()
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RemoteFile {
    #[serde(default, deserialize_with = "nd")]
    pub id: u64,
    #[serde(default, deserialize_with = "nd")]
    pub name: String,
    #[serde(default, deserialize_with = "nd")]
    pub short_name: String,
    #[serde(default, deserialize_with = "nd")]
    pub size: f64,
    #[serde(default, deserialize_with = "nd")]
    pub zipped: bool,
}

impl RemoteFile {
    pub fn file_name(&self) -> String {
        if self.short_name.is_empty() { self.name.rsplit('/').next().unwrap_or(&self.name).to_string() } else { self.short_name.clone() }
    }
}

#[derive(Clone)]
pub struct TorBox {
    http: reqwest::Client,
    key: String,
}

impl TorBox {
    pub fn new(http: reqwest::Client, key: String) -> Self {
        Self { http, key }
    }

    async fn call(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        let env: Envelope = serde_json::from_str(&text).map_err(|_| {
            let snippet: String = text.chars().take(200).collect();
            anyhow!("TorBox HTTP {status}: {snippet}")
        })?;
        if !env.success {
            bail!("TorBox {}: {}", env.error.unwrap_or_else(|| status.to_string()), env.detail.unwrap_or_default());
        }
        Ok(env.data)
    }

    /// Add a web download (direct URL) or torrent (magnet). Returns TorBox's item id.
    /// `only_if_cached`: refuse instead of starting a (possibly multi-TB) download.
    pub async fn create(&self, kind: Kind, source: &str, name: Option<&str>, only_if_cached: bool) -> Result<u64> {
        let mut form = Form::new();
        if only_if_cached {
            form = form.text("add_only_if_cached", "true");
        }
        let endpoint = match kind {
            Kind::WebDl => {
                form = form.text("link", source.to_string());
                "createwebdownload"
            }
            Kind::Torrent => {
                // keep individual files: we usually want one file out of a big torrent
                form = form.text("magnet", source.to_string()).text("allow_zip", "false");
                "createtorrent"
            }
        };
        if let Some(n) = name {
            form = form.text("name", n.to_string());
        }
        let data = self.call(self.http.post(format!("{BASE}/{}/{endpoint}", kind.path())).bearer_auth(&self.key).multipart(form)).await?;
        ["webdownload_id", "torrent_id", "id"]
            .iter()
            .find_map(|k| data.get(k).and_then(as_u64))
            .ok_or_else(|| anyhow!("TorBox create: no id in response: {data}"))
    }

    pub async fn get(&self, kind: Kind, id: u64) -> Result<Item> {
        let data = self
            .call(
                self.http
                    .get(format!("{BASE}/{}/mylist", kind.path()))
                    .bearer_auth(&self.key)
                    .query(&[("id", id.to_string()), ("bypass_cache", "true".into())]),
            )
            .await?;
        let obj = match data {
            Value::Array(mut a) if !a.is_empty() => a.swap_remove(0),
            Value::Object(_) => data,
            other => bail!("TorBox item {id} not found ({other})"),
        };
        Ok(serde_json::from_value(obj)?)
    }

    /// All items of one kind in the account (one request, used for polling).
    pub async fn list(&self, kind: Kind) -> Result<Vec<Item>> {
        let data = self
            .call(
                self.http
                    .get(format!("{BASE}/{}/mylist", kind.path()))
                    .bearer_auth(&self.key)
                    .query(&[("bypass_cache", "true"), ("limit", "1000")]),
            )
            .await?;
        match data {
            Value::Array(items) => Ok(items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect()),
            Value::Null => Ok(Vec::new()),
            other => bail!("TorBox mylist: unexpected response {other}"),
        }
    }

    /// Which of these torrents TorBox has cached, and which files of each (base names,
    /// lowercase). A hash missing from the result isn't cached at all. TorBox can cache
    /// just part of a torrent, so the file list matters for big multi-game torrents.
    pub async fn cached_files(&self, hashes: &[String]) -> Result<HashMap<String, HashSet<String>>> {
        let mut out = HashMap::new();
        for chunk in hashes.chunks(20) {
            let data = self
                .call(self.http.get(format!("{BASE}/torrents/checkcached")).bearer_auth(&self.key).query(&[
                    ("hash", chunk.join(",")),
                    ("format", "object".into()),
                    ("list_files", "true".into()),
                ]))
                .await?;
            let Value::Object(items) = data else { continue };
            for (hash, item) in items {
                let files: HashSet<String> = item
                    .get("files")
                    .and_then(|f| f.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f.get("name").and_then(|n| n.as_str()))
                    .filter(|n| !has_ext(n, "parts"))
                    .map(|n| n.rsplit('/').next().unwrap_or(n).to_lowercase())
                    .collect();
                out.insert(hash.to_lowercase(), files);
            }
        }
        Ok(out)
    }

    /// Get a temporary direct-download URL for one file of a finished item.
    pub async fn request_dl(&self, kind: Kind, id: u64, file_id: u64) -> Result<String> {
        let data = self
            .call(self.http.get(format!("{BASE}/{}/requestdl", kind.path())).query(&[
                ("token", self.key.clone()),
                (kind.id_param(), id.to_string()),
                ("file_id", file_id.to_string()),
                ("zip_link", "false".into()),
            ]))
            .await?;
        match data {
            Value::String(s) => Ok(s),
            other => other
                .get("url")
                .or_else(|| other.get("link"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("TorBox requestdl: unexpected response {other}")),
        }
    }
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "network + TorBox key from the config"]
    async fn cached_files_live() {
        let paths = crate::config::Paths::new().unwrap();
        let key = crate::config::Config::load(&paths.config_file).unwrap().api_key().expect("torbox key");
        let tb = TorBox::new(reqwest::Client::new(), key);
        let ps3 = "626a5e05f498e435145c97352aa2fecf7e95a668".to_string();
        let snes = "93e0d2060d1b05f5b19d0a9e40011b51a76a933c".to_string();
        let got = tb.cached_files(&[ps3.clone(), snes.clone()]).await.unwrap();
        for (h, f) in &got {
            println!("{h}: {} files, e.g. {:?}", f.len(), f.iter().next());
        }
        assert!(got.get(&ps3).is_some_and(|f| f.len() < 10), "PS3 is only partly cached");
    }

    #[test]
    fn spots_partial_torrents() {
        let item: Item = serde_json::from_value(serde_json::json!({
            "id": 1, "hash": "c98f", "download_state": "cached", "download_present": true, "allow_zipped": true,
            "files": [
                {"id": 0, "name": ".c98f.parts", "short_name": ".c98f.parts"},
                {"id": 1, "name": "Minerva_Myrient/Redump/Microsoft - Xbox 360/Table Tennis (USA).zip", "short_name": "Table Tennis (USA).zip"}
            ]
        }))
        .unwrap();
        assert!(item.is_partial() && !item.is_zipped());
        assert_eq!(item.real_files(), 1);
        assert!(item.has_file("table tennis (usa).zip", Some(800)));
        assert!(!item.has_file("Blitz - The League II (USA, Europe).zip", Some(452)));
        assert!(!item.has_file("Whatever (USA).zip", Some(1))); // ids are renumbered in a partial item
    }

    #[test]
    fn parses_item_with_nulls() {
        let v = serde_json::json!({
            "id": 42, "name": "x.zip", "size": 100, "progress": 0.5,
            "download_state": null, "download_present": true,
            "files": [{"id": 0, "name": "x/x.zip", "short_name": null, "size": 100}]
        });
        let item: Item = serde_json::from_value(v).unwrap();
        assert_eq!(item.id, 42);
        assert!(!item.is_zipped());
        let zipped: Item = serde_json::from_value(serde_json::json!({
            "id": 1, "allow_zipped": true,
            "files": [{"id": 0, "name": "Minerva_Myrient/Minerva_Myrient.zip", "short_name": "Minerva_Myrient.zip", "zipped": true}]
        }))
        .unwrap();
        assert!(zipped.is_zipped());
        assert!(item.download_present);
        assert_eq!(item.files[0].file_name(), "x.zip");
    }
}
