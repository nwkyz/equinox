//! Image storage: downloads go to `~/Pictures/Equinox/{source}/`, named
//! `{time}_{content hash}`, with a same-named JSON description.
//!
//! Example:
//! ```text
//! ~/Pictures/Equinox/bing/20260804-081530_1f40c46e320e.jpg
//! ~/Pictures/Equinox/bing/20260804-081530_1f40c46e320e.json   # title/copyright/summary etc.
//! ```
//!
//! The content hash in the file name (first 12 hex chars of sha256) guarantees
//! **one copy per image**: repeated updates of the same source on the same day
//! (scheduled + manual) that fetch identical content reuse the existing file
//! instead of storing a duplicate.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use gettextrs::gettext;
use serde::{Deserialize, Serialize};

use crate::http::{HttpClient, image_extension};
use crate::source::FetchResult;

/// Length of the content hash in file names (hex chars).
const HASH_LEN: usize = 12;

/// Longest edge of a thumbnail (px). The gallery/history grid and carousel
/// use it to avoid decoding full-size images.
pub const THUMB_MAX: i32 = 512;

/// Thumbnail JPEG quality.
pub const THUMB_QUALITY: &str = "82";

/// Thumbnail path convention: `{original name minus extension}_thumb.jpg`.
/// Original `20260803_abc123.jpg` → `20260803_abc123_thumb.jpg`.
fn thumbnail_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    dir.join(format!("{stem}_thumb.jpg"))
}

/// Original pixel size via a **header-only** read (`imagesize`, pure Rust)
/// — no decoder is linked, so this works in the daemon as well as the GUI.
/// Returns (0, 0) when the format is unknown/unreadable.
pub fn image_dimensions(path: &Path) -> (i32, i32) {
    match imagesize::size(path) {
        Ok(s) => (s.width as i32, s.height as i32),
        Err(_) => (0, 0),
    }
}

/// Description metadata of an image, stored as JSON next to the image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageMeta {
    pub source: String,
    pub title: String,
    pub copyright: Option<String>,
    pub url: String,
    /// Unix timestamp.
    pub downloaded_at: i64,
    /// Content hash (first 12 hex chars of sha256); missing in old data,
    /// defaults to empty string.
    #[serde(default)]
    pub hash: String,
    /// The image's **original** date ("YYYY-MM-DD"); "" when the source
    /// provides none, missing in old data defaults to "".
    #[serde(default)]
    pub date: String,
    /// Original pixel size (written at download time); 0 in legacy data.
    /// Lets the GUI know the aspect ratio without decoding anything.
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    pub extra: HashMap<String, String>,
}

/// A stored image.
#[derive(Debug, Clone)]
pub struct StoredImage {
    pub path: PathBuf,
    pub json_path: PathBuf,
    pub meta: ImageMeta,
    /// True when the fetch matched something already on disk (content hash
    /// or a source identity like Spotlight's entityId) and NO new file was
    /// added — callers use this to skip "new image" notifications.
    pub reused: bool,
}

pub struct Storage;

impl Storage {
    /// Thumbnail path convention: `{original name minus extension}_thumb.jpg`.
    /// Original `20260803_abc123.jpg` → `20260803_abc123_thumb.jpg`.
    pub fn thumbnail_path(path: &Path) -> PathBuf {
        thumbnail_path(path)
    }

    /// Storage root: `~/Pictures/Equinox`.
    pub fn root() -> PathBuf {
        glib::user_special_dir(glib::UserDirectory::Pictures)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
            })
            .join("Equinox")
    }

    /// A source's image directory.
    pub fn source_dir(source: &str) -> PathBuf {
        Self::root().join(source)
    }

    /// Fetch and store: `{image date or download time}_{content hash}.{ext}`
    /// + same-named `.json` description.
    ///
    /// Sources that provide the image's own date name by it
    /// (Bing/NASA/POTD), otherwise the download time is used. When the
    /// content already exists in the directory, the existing file is reused
    /// (dedup).
    pub async fn store(
        http: &HttpClient,
        source: &str,
        result: &FetchResult,
    ) -> Result<StoredImage> {
        // Identity dedup BEFORE downloading: when the fetch carries a stable
        // source identity ("dedup-id" in extra, e.g. Spotlight's entityId)
        // that matches an already-stored image, reuse that file — the same
        // picture may otherwise come back with a different CDN URL or
        // re-encoded bytes and slip past the content-hash check below.
        if let Some(id) = result.extra.get("dedup-id").filter(|v| !v.is_empty()) {
            let dir = Self::source_dir(source);
            if let Some(existing) = find_existing_by_dedup_id(&dir, id) {
                log::info!(
                    "fetched image already stored (dedup-id), skipping download: {}",
                    existing.display()
                );
                return Ok(StoredImage {
                    path: existing.clone(),
                    json_path: existing.with_extension("json"),
                    meta: ImageMeta {
                        source: source.to_owned(),
                        title: result.title.clone(),
                        copyright: result.copyright.clone(),
                        url: result.image_url.clone(),
                        downloaded_at: chrono::Local::now().timestamp(),
                        hash: String::new(),
                        date: result
                            .date
                            .map(|d| d.format("%Y-%m-%d").to_string())
                            .unwrap_or_default(),
                        width: 0,
                        height: 0,
                        extra: result.extra.clone(),
                    },
                    reused: true,
                });
            }
        }
        let resp = http.get(&result.image_url).await?;
        if resp.status != soup::Status::Ok {
            // Diagnostic message; status codes and URLs are not translatable.
            return Err(anyhow!(
                "HTTP {status:?}: {url}",
                status = resp.status,
                url = result.image_url
            ));
        }
        // Reject responses that explicitly declare a non-image type: some
        // providers serve an HTML landing/protection page or a PDF with HTTP
        // 200 (observed with Europeana's `edmIsShownBy`). Without this guard
        // the bytes would be saved under an image extension and every view of
        // the file would fail. A missing Content-Type is left to the
        // URL-extension fallback below.
        if !is_image_content_type(resp.content_type.as_deref()) {
            let ct = resp.content_type.as_deref().unwrap_or("").to_owned();
            return Err(anyhow!(
                "{} ({ct}): {url}",
                gettext("Response is not an image"),
                url = result.image_url
            ));
        }
        let ext = image_extension(resp.content_type.as_deref(), &result.image_url);
        let meta = ImageMeta {
            source: source.to_owned(),
            title: result.title.clone(),
            copyright: result.copyright.clone(),
            url: result.image_url.clone(),
            downloaded_at: chrono::Local::now().timestamp(),
            hash: content_hash(&resp.body),
            date: result
                .date
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            width: 0,
            height: 0,
            extra: result.extra.clone(),
        };
        let stored = store_body_at(&Self::root(), source, &ext, &meta, &resp.body)?;
        // Record the original pixel size in the sidecar (header-only read),
        // so the GUI knows aspect ratios without decoding.
        let (w, h) = image_dimensions(&stored.path);
        if w > 0 && h > 0 {
            let mut m = meta.clone();
            m.width = w;
            m.height = h;
            let _ = fs::write(
                stored.json_path.clone(),
                serde_json::to_string_pretty(&m).unwrap_or_default(),
            );
        }
        // No thumbnail generation here: the daemon links no image decoders
        // (GUI backfills missing thumbnails on load).
        Ok(stored)
    }

    /// List all images of a source (file name descending = image date /
    /// download time, newest first). Old data without a description file is
    /// skipped; old images lacking a thumbnail get one generated here.
    /// Legacy backfill: sidecars without dimensions get a one-time header
    /// read + rewrite, so GUIs never need to decode for ratios.
    fn backfill_dimensions(meta_path: &Path) {
        let ok = std::fs::read_to_string(meta_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<ImageMeta>(&raw).ok());
        if let Some(mut m) = ok {
            if m.width == 0 || m.height == 0 {
                let img = meta_path.with_extension("");
                let (w, h) = image_dimensions(&img);
                if w > 0 && h > 0 {
                    m.width = w;
                    m.height = h;
                    let _ = std::fs::write(
                        meta_path,
                        serde_json::to_string_pretty(&m).unwrap_or_default(),
                    );
                }
            }
        }
    }

    pub fn list(source: &str) -> Vec<StoredImage> {
        let dir = Self::source_dir(source);
        let images = list_dir(&dir);
        // Thumbnails are NOT generated here: the daemon links no image
        // decoders, and generating them on every scan would block the
        // caller (the daemon's main loop, and with it every D-Bus call plus
        // the update task itself) for seconds on legacy data. The GUI
        // backfills missing thumbnails in the background when it loads a
        // view; until then an image without a thumbnail simply falls back
        // to the original in the GUI (`display_path`).
        for img in &images {
            if img.meta.width == 0 || img.meta.height == 0 {
                Self::backfill_dimensions(&img.json_path);
            }
        }
        images
    }

    /// Latest image of a source.
    /// Whether this source already stores an image dated `date` (filename
    /// prefix `YYYYMMDD`). Cheap directory scan, no file reads — used to skip
    /// re-fetching backfill days that are already on disk.
    pub fn has_dated(source: &str, date: chrono::NaiveDate) -> bool {
        let dir = Self::source_dir(source);
        let prefix = date.format("%Y%m%d").to_string();
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            })
            .unwrap_or(false)
    }

    pub fn latest(source: &str) -> Option<StoredImage> {
        Self::list(source).into_iter().next()
    }

    /// Random image of a source.
    pub fn random(source: &str) -> Option<StoredImage> {
        let list = Self::list(source);
        if list.is_empty() {
            return None;
        }
        let idx = rand::random_range(0..list.len());
        list.into_iter().nth(idx)
    }

    /// Delete an image and its description file.
    pub fn delete(path: &Path) -> Result<()> {
        fs::remove_file(path)
            .with_context(|| format!("{}: {}", gettext("Failed to delete"), path.display()))?;
        let json_path = path.with_extension("json");
        if json_path.exists() {
            fs::remove_file(&json_path)?;
        }
        let thumb = thumbnail_path(path);
        if thumb.exists() {
            fs::remove_file(thumb)?;
        }
        Ok(())
    }

    /// Keep at most `max` images per source (newest first); delete the
    /// overflow (image + sidecar + thumbnail). Returns the removed count.
    pub fn trim(source: &str, max: usize) -> usize {
        if max == 0 {
            return 0; // 0 = unlimited
        }
        let images = Self::list(source);
        let mut removed = 0;
        for img in images.iter().skip(max) {
            if Self::delete(&img.path).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// Delete all images of a source.
    pub fn delete_all(source: &str) -> Result<usize> {
        let dir = Self::source_dir(source);
        if !dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_file() {
                fs::remove_file(&path)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Read an image's description metadata (None when missing/unparsable).
    pub fn meta_for(path: &Path) -> Option<ImageMeta> {
        fs::read_to_string(path.with_extension("json"))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
    }
}

/// Whether a response Content-Type denotes an image. A missing header is
/// accepted (the URL extension is used as a fallback); any explicit non-image
/// type (`text/html`, `application/pdf`, …) is rejected — see `store`.
fn is_image_content_type(content_type: Option<&str>) -> bool {
    match content_type {
        None => true,
        Some(ct) => ct
            .split(';')
            .next()
            .unwrap_or(ct)
            .trim()
            .to_ascii_lowercase()
            .starts_with("image/"),
    }
}

/// Content hash: first `HASH_LEN` hex chars of sha256 (used for file-name dedup).
fn content_hash(body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(body);
    digest
        .iter()
        .take(HASH_LEN / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Scan a directory for an existing image whose name carries `_{hash}`
/// (content dedup).
fn find_existing_by_hash(dir: &Path, hash: &str) -> Option<PathBuf> {
    let suffix = format!("_{hash}.");
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains(&suffix) && !name.ends_with(".json") {
            return Some(entry.path());
        }
    }
    None
}

/// Scan a source's sidecar JSONs for one carrying `extra["dedup-id"] == id`
/// and return its image path (identity dedup across re-encoded variants).
fn find_existing_by_dedup_id(dir: &Path, id: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let json_path = entry.path();
        if json_path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let hit = fs::read_to_string(&json_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<ImageMeta>(&raw).ok())
            .is_some_and(|m| m.extra.get("dedup-id").map(String::as_str) == Some(id));
        if hit {
            // Sidecar name replaces the image extension (`x.jpg` → `x.json`);
            // find the image file sharing this stem.
            let stem = json_path.with_extension("");
            return fs::read_dir(dir)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    *p != json_path
                        && p.with_extension("json") == json_path
                        && p.file_stem().and_then(|s| s.to_str())
                            == stem.file_stem().and_then(|s| s.to_str())
                });
        }
    }
    None
}

/// Core write: `{prefix}_{hash}.{ext}` + same-named JSON; existing content is
/// reused. Prefix = the image's original date (YYYYMMDD) or the download time;
/// different content under the same prefix gets -1, -2 appended.
/// `root` is the storage root (tests inject a temp dir; production passes
/// `Storage::root()`).
fn store_body_at(
    root: &Path,
    source: &str,
    ext: &str,
    meta: &ImageMeta,
    body: &[u8],
) -> Result<StoredImage> {
    let dir = root.join(source);
    fs::create_dir_all(&dir).with_context(|| {
        format!("{}: {}", gettext("Failed to create directory"), dir.display())
    })?;

    // Content dedup: reuse an existing file with the same hash
    if let Some(existing) = find_existing_by_hash(&dir, &meta.hash) {
        let json_path = existing.with_extension("json");
        if !json_path.exists() {
            // Image present but description lost: rewrite the description
            fs::write(&json_path, serde_json::to_string_pretty(meta)?)?;
        }
        return Ok(StoredImage {
            path: existing,
            json_path,
            meta: meta.clone(),
            reused: true,
        });
    }

    // Identity dedup: sources may serve the same picture again with
    // re-encoded bytes (different hash). When the fetch carries a stable
    // identity ("dedup-id" in extra, e.g. Spotlight's entityId) matching an
    // already-stored image, reuse that file instead of adding a duplicate.
    if let Some(id) = meta.extra.get("dedup-id").filter(|v| !v.is_empty()) {
        if let Some(existing) = find_existing_by_dedup_id(&dir, id) {
            log::info!("content identical to stored image (dedup-id), reusing: {}",
                existing.display());
            return Ok(StoredImage {
                json_path: existing.with_extension("json"),
                path: existing,
                meta: meta.clone(),
                reused: true,
            });
        }
    }

    // Naming prefix: the image date when present, otherwise the download time
    let prefix = if meta.date.is_empty() {
        chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
    } else {
        meta.date.replace('-', "")
    };
    let mut path = dir.join(format!("{prefix}_{}.{ext}", meta.hash));
    let mut n = 1u32;
    while path.exists() {
        path = dir.join(format!("{prefix}-{n}_{}.{ext}", meta.hash));
        n += 1;
    }
    fs::write(&path, body)
        .with_context(|| format!("{}: {}", gettext("Failed to write image"), path.display()))?;

    let json_path = path.with_extension("json");
    fs::write(&json_path, serde_json::to_string_pretty(meta)?).with_context(|| {
        format!(
            "{}: {}",
            gettext("Failed to write description"),
            json_path.display()
        )
    })?;

    Ok(StoredImage {
        path,
        json_path,
        meta: meta.clone(),
        reused: false,
    })
}

/// List images in a directory (file name descending = newest first).
fn list_dir(dir: &Path) -> Vec<StoredImage> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut images: Vec<StoredImage> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "avif" | "gif" | "tif" | "tiff") {
            continue;
        }
        let json_path = path.with_extension("json");
        let Ok(raw) = fs::read_to_string(&json_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<ImageMeta>(&raw) else {
            continue;
        };
        images.push(StoredImage { path, json_path, meta, reused: false });
    }
    // Sort by the image's own date descending (newest first): the original
    // date when present, the download date as fallback (old data / undated
    // sources); within the same day, download time descending keeps order stable.
    images.sort_by_key(|img| std::cmp::Reverse(sort_key(img)));
    images
}

/// Sort key: original date (YYYY-MM-DD) or download date fallback, plus the
/// download timestamp for a stable order within the same day.
fn sort_key(img: &StoredImage) -> (String, i64) {
    let date = if img.meta.date.is_empty() {
        chrono::DateTime::from_timestamp(img.meta.downloaded_at, 0)
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    } else {
        img.meta.date.clone()
    };
    (date, img.meta.downloaded_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn test_root_for(name: &str) -> PathBuf {
        env::temp_dir().join(format!("equinox-storage-{}-{}", std::process::id(), name))
    }

    fn meta(source: &str, title: &str, body: &[u8]) -> ImageMeta {
        ImageMeta {
            source: source.into(),
            title: title.into(),
            copyright: None,
            url: "https://x".into(),
            downloaded_at: 0,
            hash: content_hash(body),
            date: String::new(),
            width: 0,
            height: 0,
            extra: Default::default(),
        }
    }

    #[test]
    fn hash_is_sha256_prefix() {
        // First 12 hex chars of sha256("hello")
        assert_eq!(content_hash(b"hello"), "2cf24dba5fb0");
    }

    #[test]
    fn accepts_only_image_content_types() {
        assert!(is_image_content_type(Some("image/jpeg")));
        assert!(is_image_content_type(Some("image/jpg")));
        assert!(is_image_content_type(Some("image/webp; charset=binary")));
        assert!(is_image_content_type(Some("IMAGE/PNG")));
        // Missing header → fall back to the URL extension in store()
        assert!(is_image_content_type(None));
        // Explicit non-image types must be rejected (Europeana serves these
        // with HTTP 200 for some records).
        assert!(!is_image_content_type(Some("text/html; charset=utf-8")));
        assert!(!is_image_content_type(Some("application/pdf")));
        assert!(!is_image_content_type(Some("application/json")));
    }

    #[test]
    fn store_dedupes_same_content() {
        let root = test_root_for("dedupe");
        let dir = root.join("bing");
        let body = b"same-image-bytes";
        let m1 = meta("bing", "First image", body);
        let m2 = meta("bing", "Second (same content)", body);

        let s1 = store_body_at(&root, "bing", "jpg", &m1, body).unwrap();
        let s2 = store_body_at(&root, "bing", "jpg", &m2, body).unwrap();

        // Same content → same file, no second copy
        assert_eq!(s1.path, s2.path);
        assert_eq!(list_dir(&dir).len(), 1);
        // File name contains time + hash
        let name = s1.path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.contains(&format!("_{}.jpg", content_hash(body))), "file name: {name}");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn store_keeps_different_content() {
        let root = test_root_for("distinct");
        let dir = root.join("bing");
        let m1 = meta("bing", "a", b"content-a");
        let m2 = meta("bing", "b", b"content-b");
        store_body_at(&root, "bing", "jpg", &m1, b"content-a").unwrap();
        store_body_at(&root, "bing", "jpg", &m2, b"content-b").unwrap();
        assert_eq!(list_dir(&dir).len(), 2);
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn dedup_id_reuses_existing_image_across_reencoded_bytes() {
        let root = test_root_for("dedup-id");
        let dir = root.join("spotlight");
        let mut m1 = meta("spotlight", "First fetch", b"bytes-v1");
        m1.extra.insert("dedup-id".into(), "128000000005722069".into());
        let s1 = store_body_at(&root, "spotlight", "jpg", &m1, b"bytes-v1").unwrap();
        assert!(!s1.reused);

        // Same creative served again, re-encoded (different hash) but the
        // same source identity → must NOT add a second file
        let mut m2 = meta("spotlight", "Refetch (re-encoded)", b"different-bytes-v2");
        m2.extra.insert("dedup-id".into(), "128000000005722069".into());
        let s2 = store_body_at(&root, "spotlight", "jpg", &m2, b"different-bytes-v2").unwrap();
        assert!(s2.reused);
        assert_eq!(s1.path, s2.path);
        assert_eq!(list_dir(&dir).len(), 1);

        // A different identity still stores normally
        let mut m3 = meta("spotlight", "Other picture", b"bytes-v3");
        m3.extra.insert("dedup-id".into(), "128000000009999999".into());
        let s3 = store_body_at(&root, "spotlight", "jpg", &m3, b"bytes-v3").unwrap();
        assert!(!s3.reused);
        assert_eq!(list_dir(&dir).len(), 2);
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn list_sorts_by_photo_date_newest_first() {
        let root = test_root_for("list");
        let dir = root.join("bing");
        fs::create_dir_all(&dir).unwrap();
        // File names intentionally unrelated to photo dates (out of order);
        // sorting must only look at meta.date
        for (name, hash, date, dl) in [
            ("20260804-090000", "333333333333", "2026-08-03", 90000),
            ("20260803-100000", "111111111111", "2026-08-04", 100000),
            ("20260804-080000", "222222222222", "2026-08-04", 80000),
        ] {
            fs::write(dir.join(format!("{name}_{hash}.jpg")), b"x").unwrap();
            fs::write(
                dir.join(format!("{name}_{hash}.json")),
                serde_json::to_string(&ImageMeta {
                    source: "bing".into(),
                    title: name.into(),
                    copyright: None,
                    url: "https://x".into(),
                    downloaded_at: dl,
                    hash: hash.into(),
                    date: date.into(),
                    width: 0,
                    height: 0,
                    extra: Default::default(),
                })
                .unwrap(),
            )
            .unwrap();
        }
        let files = list_dir(&dir);
        let names: Vec<String> = files
            .iter()
            .map(|i| i.path.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        // Photo dates 08-04 first (same day: download time descending),
        // 08-03 last — independent of file names
        assert_eq!(
            names,
            vec![
                "20260803-100000_111111111111".to_string(),
                "20260804-080000_222222222222".to_string(),
                "20260804-090000_333333333333".to_string(),
            ]
        );
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn list_mixed_dated_and_legacy_sorts_by_date_with_fallback() {
        let root = test_root_for("mixed");
        let dir = root.join("bing");
        fs::create_dir_all(&dir).unwrap();
        // Legacy data (no date): sorted by download date 08-05
        let mut legacy = meta("bing", "Legacy image", b"legacy");
        legacy.downloaded_at = 1_785_888_000; // 2026-08-05 00:00 UTC
        fs::write(dir.join("legacy_hash.jpg"), b"x").unwrap();
        fs::write(
            dir.join("legacy_hash.json"),
            serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
        // New data (dated): 08-04 / 08-06
        for (hash, date) in [("aa", "2026-08-04"), ("bb", "2026-08-06")] {
            let mut m = meta("bing", "New image", hash.as_bytes());
            m.date = date.into();
            fs::write(dir.join(format!("{}_{}.jpg", date.replace('-', ""), hash)), b"x").unwrap();
            fs::write(
                dir.join(format!("{}_{}.jpg", date.replace('-', ""), hash).replace(".jpg", ".json")),
                serde_json::to_string(&m).unwrap(),
            )
            .unwrap();
        }
        let dates: Vec<String> = list_dir(&dir)
            .iter()
            .map(|i| {
                if i.meta.date.is_empty() {
                    "legacy(download date)".to_owned()
                } else {
                    i.meta.date.clone()
                }
            })
            .collect();
        // 08-06 > 08-05 (legacy fallback) > 08-04
        assert_eq!(
            dates,
            vec![
                "2026-08-06".to_string(),
                "legacy(download date)".to_string(),
                "2026-08-04".to_string(),
            ]
        );
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn list_skips_images_without_meta() {
        let root = test_root_for("skip");
        let dir = root.join("nasa");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("20260804-080000_111111111111.jpg"), b"x").unwrap(); // no json, skipped
        fs::write(dir.join("20260804-090000_222222222222.jpg"), b"x").unwrap();
        fs::write(
            dir.join("20260804-090000_222222222222.json"),
            serde_json::to_string(&meta("nasa", "t", b"x")).unwrap(),
        )
        .unwrap();
        assert_eq!(list_dir(&dir).len(), 1);
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn delete_removes_json_too() {
        let root = test_root_for("del");
        let dir = root.join("wikimedia");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("20260804-080000_111111111111.jpg"), b"x").unwrap();
        fs::write(dir.join("20260804-080000_111111111111.json"), b"{}").unwrap();
        Storage::delete(&dir.join("20260804-080000_111111111111.jpg")).unwrap();
        assert!(!dir.join("20260804-080000_111111111111.jpg").exists());
        assert!(!dir.join("20260804-080000_111111111111.json").exists());
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn store_names_by_image_date_not_download_time() {
        let root = test_root_for("date");
        let dir = root.join("bing");
        let body = b"dated-image-bytes";
        let mut m = meta("bing", "Dated image", body);
        m.date = "2026-08-03".into(); // e.g. Bing startdate

        let s = store_body_at(&root, "bing", "jpg", &m, body).unwrap();
        let name = s.path.file_name().unwrap().to_string_lossy().into_owned();
        // File-name prefix is the original date 20260803, not the download
        // timestamp (YYYYMMDD-HHMMSS shaped)
        assert!(name.starts_with("20260803_"), "file name: {name}");

        // Undated still uses the download-time shape
        let s2 = store_body_at(
            &root,
            "bing",
            "jpg",
            &meta("bing", "Undated", b"other-bytes"),
            b"other-bytes",
        )
        .unwrap();
        let name2 = s2.path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name2.contains("-"), "undated should contain the timestamp separator: {name2}");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn old_json_without_hash_still_parses() {
        // Compatibility: json stored before the hash field existed still parses
        let raw = r#"{"source":"bing","title":"Legacy data","copyright":null,"url":"https://x","downloaded_at":0,"extra":{}}"#;
        let m: ImageMeta = serde_json::from_str(raw).unwrap();
        assert_eq!(m.hash, "");
    }
}
