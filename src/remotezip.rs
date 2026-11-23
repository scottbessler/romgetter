//! Pull one file out of a remote zip using HTTP range requests.
//!
//! TorBox packs torrents with 100+ files into a single zip when the account's
//! "allow zipped" setting is on (it overrides the per-request flag). A Minerva system
//! folder becomes e.g. a 15 GB zip, but its files are stored uncompressed, so we read
//! the central directory from the end and copy just the one entry's bytes.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub method: u16,
    pub compressed: u64,
    pub size: u64,
    pub local_header: u64,
}

impl Entry {
    pub fn base_name(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

async fn range(http: &reqwest::Client, url: &str, spec: &str) -> Result<(Vec<u8>, Option<u64>)> {
    let resp = http.get(url).header(RANGE, format!("bytes={spec}")).send().await?.error_for_status()?;
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        bail!("server ignored the range request ({}); can't read inside the zip", resp.status());
    }
    let total =
        resp.headers().get(CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.rsplit('/').next()).and_then(|v| v.parse().ok());
    Ok((resp.bytes().await?.to_vec(), total))
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// (central directory offset, size) from the archive's tail.
fn locate_cd(tail: &[u8]) -> Result<(u64, u64)> {
    let eocd = rfind(tail, b"PK\x05\x06").ok_or_else(|| anyhow!("not a zip (no end-of-central-directory record)"))?;
    if tail.len() < eocd + 22 {
        bail!("truncated zip footer");
    }
    let (size32, off32) = (u32le(tail, eocd + 12), u32le(tail, eocd + 16));
    if size32 != u32::MAX && off32 != u32::MAX {
        return Ok((u64::from(off32), u64::from(size32)));
    }
    // zip64: the zip64 end record sits just before the locator
    let z = rfind(&tail[..eocd], b"PK\x06\x06").ok_or_else(|| anyhow!("zip64 archive without a zip64 end record"))?;
    if tail.len() < z + 56 {
        bail!("truncated zip64 record");
    }
    Ok((u64le(tail, z + 48), u64le(tail, z + 40)))
}

fn parse_cd(cd: &[u8]) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 46 <= cd.len() && &cd[p..p + 4] == b"PK\x01\x02" {
        let method = u16le(cd, p + 10);
        let mut compressed = u64::from(u32le(cd, p + 20));
        let mut size = u64::from(u32le(cd, p + 24));
        let (nl, xl, cl) = (u16le(cd, p + 28) as usize, u16le(cd, p + 30) as usize, u16le(cd, p + 32) as usize);
        let mut local_header = u64::from(u32le(cd, p + 42));
        let end = p + 46 + nl + xl + cl;
        if end > cd.len() {
            bail!("truncated central directory");
        }
        let name = String::from_utf8_lossy(&cd[p + 46..p + 46 + nl]).into_owned();
        // zip64 extra field: the 32-bit fields that are 0xFFFFFFFF follow, in this order
        let mut x = p + 46 + nl;
        let xend = x + xl;
        while x + 4 <= xend {
            let (id, len) = (u16le(cd, x), u16le(cd, x + 2) as usize);
            if id == 0x0001 {
                let mut q = x + 4;
                for field in [&mut size, &mut compressed, &mut local_header] {
                    if *field == u64::from(u32::MAX) && q + 8 <= x + 4 + len {
                        *field = u64le(cd, q);
                        q += 8;
                    }
                }
            }
            x += 4 + len;
        }
        out.push(Entry { name, method, compressed, size, local_header });
        p = end;
    }
    Ok(out)
}

/// Compression methods `extract` can read: stored and deflate.
pub fn is_supported(method: u16) -> bool {
    matches!(method, 0 | 8)
}

/// List a remote zip's entries (two small range requests).
pub async fn list(http: &reqwest::Client, url: &str) -> Result<Vec<Entry>> {
    list_sized(http, url).await.map(|(e, _)| e)
}

/// Like `list`, also returning the zip's size as served (from Content-Range).
pub async fn list_sized(http: &reqwest::Client, url: &str) -> Result<(Vec<Entry>, Option<u64>)> {
    let (tail, total) = range(http, url, "-65558").await.context("reading zip footer")?;
    Ok((list_from_tail(http, url, &tail).await?, total))
}

/// Just the served size of a file (one tiny range request).
pub async fn served_size(http: &reqwest::Client, url: &str) -> Result<Option<u64>> {
    Ok(range(http, url, "0-0").await?.1)
}

async fn list_from_tail(http: &reqwest::Client, url: &str, tail: &[u8]) -> Result<Vec<Entry>> {
    let (cd_off, cd_size) = locate_cd(tail)?;
    let (cd, _) = range(http, url, &format!("{cd_off}-{}", cd_off + cd_size - 1)).await.context("reading zip directory")?;
    parse_cd(&cd)
}

/// Copy one entry to `dest`, stored or deflated, reporting (done, total) compressed bytes.
pub async fn extract(http: &reqwest::Client, url: &str, e: &Entry, dest: &Path, progress: &impl Fn(u64, u64)) -> Result<()> {
    if !is_supported(e.method) {
        bail!("'{}' uses zip compression method {}; only stored and deflated entries are supported", e.name, e.method);
    }
    let (lh, _) = range(http, url, &format!("{}-{}", e.local_header, e.local_header + 29)).await?;
    if lh.len() < 30 || &lh[..4] != b"PK\x03\x04" {
        bail!("bad local header for '{}'", e.name);
    }
    let start = e.local_header + 30 + u64::from(u16le(&lh, 26)) + u64::from(u16le(&lh, 28));
    let part = dest.with_extension("part");
    let mut out = tokio::fs::File::create(&part).await.with_context(|| format!("creating {}", part.display()))?;
    let mut inflate = (e.method == 8).then(|| flate2::write::DeflateDecoder::new(Vec::new()));
    if e.compressed > 0 {
        let resp = http.get(url).header(RANGE, format!("bytes={start}-{}", start + e.compressed - 1)).send().await?.error_for_status()?;
        let mut stream = resp.bytes_stream();
        let (mut done, mut last) = (0u64, Instant::now());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            match inflate.as_mut() {
                Some(d) => {
                    d.write_all(&chunk).with_context(|| format!("inflating '{}'", e.name))?;
                    out.write_all(d.get_ref()).await?;
                    d.get_mut().clear();
                }
                None => out.write_all(&chunk).await?,
            }
            done += chunk.len() as u64;
            if last.elapsed() > Duration::from_millis(250) {
                progress(done, e.compressed);
                last = Instant::now();
            }
        }
        if done != e.compressed {
            bail!("short read: got {done} of {} bytes", e.compressed);
        }
    }
    if let Some(d) = inflate {
        out.write_all(&d.finish().with_context(|| format!("inflating '{}'", e.name))?).await?;
    }
    out.flush().await?;
    drop(out);
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small stored zip in memory (optionally with zip64-style 0xFFFFFFFF fields).
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data) in files {
            let lho = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&0u32.to_le_bytes()); // crc (unchecked)
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            // central entry with a zip64 extra carrying the local header offset
            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&12u16.to_le_bytes()); // extra len
            cd.extend_from_slice(&[0; 10]); // comment len, disk, internal + external attrs
            cd.extend_from_slice(&u32::MAX.to_le_bytes()); // lho in zip64 extra
            cd.extend_from_slice(name.as_bytes());
            cd.extend_from_slice(&1u16.to_le_bytes());
            cd.extend_from_slice(&8u16.to_le_bytes());
            cd.extend_from_slice(&u64::from(lho).to_le_bytes());
        }
        let cd_off = out.len() as u64;
        out.extend_from_slice(&cd);
        // zip64 end record + locator + classic EOCD with 0xFFFFFFFF markers
        let z = out.len() as u64;
        out.extend_from_slice(b"PK\x06\x06");
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u64).to_le_bytes());
        out.extend_from_slice(&(files.len() as u64).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u64).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(b"PK\x06\x07");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&z.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]);
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// `ROMGETTER_ZIP_URL=<url> ROMGETTER_ZIP_PICK=<file> ROMGETTER_ZIP_OUT=<path> cargo test -- --ignored remote_zip_live`
    #[tokio::test]
    #[ignore = "network"]
    async fn remote_zip_live() {
        let (url, pick, out) = (
            std::env::var("ROMGETTER_ZIP_URL").unwrap(),
            std::env::var("ROMGETTER_ZIP_PICK").unwrap(),
            std::env::var("ROMGETTER_ZIP_OUT").unwrap(),
        );
        let http = reqwest::Client::new();
        let entries = list(&http, &url).await.unwrap();
        let e = entries.iter().find(|e| e.base_name() == pick).unwrap();
        extract(&http, &url, e, Path::new(&out), &|_, _| {}).await.unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().len(), e.size);
    }

    #[test]
    fn reads_zip64_directory() {
        let zip = make_zip(&[("a/One.zip", b"hello"), ("a/GoldenEye 007 (USA).zip", b"0123456789")]);
        let tail = &zip[zip.len().saturating_sub(65558)..];
        let (off, size) = locate_cd(tail).unwrap();
        let entries = parse_cd(&zip[off as usize..(off + size) as usize]).unwrap();
        assert_eq!(entries.len(), 2);
        let e = &entries[1];
        assert_eq!(e.base_name(), "GoldenEye 007 (USA).zip");
        assert_eq!(e.size, 10);
        let lh = e.local_header as usize;
        assert_eq!(&zip[lh..lh + 4], b"PK\x03\x04");
        let start = lh + 30 + u16le(&zip, lh + 26) as usize + u16le(&zip, lh + 28) as usize;
        assert_eq!(&zip[start..start + 10], b"0123456789");
    }
}
