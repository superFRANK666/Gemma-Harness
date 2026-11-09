//! Disk-backed artifacts keep image payloads out of persisted conversation JSON.
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use harness_model::ChatMessage;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

/// Content-addressed, lossless text for tool previews with a shared text budget.
pub fn save_text(root: &Path, text: &str) -> Result<String> {
    if text.len() > MAX_ARTIFACT_BYTES {
        bail!("Text artifact exceeds the 16 MiB size limit")
    }
    let name = format!("{}.json", digest(text.as_bytes()));
    store_image(root, &name, text.as_bytes())?;
    Ok(name)
}

/// Return a persistence copy. The caller's active multimodal message is unchanged.
pub fn offload_message(message: &ChatMessage, artifact_dir: &Path) -> Result<ChatMessage> {
    let mut persisted = message.clone();
    let Some(parts) = persisted.content.as_mut().and_then(Value::as_array_mut) else {
        return Ok(persisted);
    };
    for part in parts {
        if part.get("type").and_then(Value::as_str) != Some("image_url") {
            continue;
        }
        let url = part
            .pointer("/image_url/url")
            .and_then(Value::as_str)
            .or_else(|| part.get("image_url").and_then(Value::as_str))
            .context("Image content part must contain an image_url")?;
        if !url
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
        {
            continue;
        }
        let (mime, bytes) = decode_image_uri(url)?;
        let sha256 = digest(&bytes);
        let extension = if mime == "image/png" { "png" } else { "jpg" };
        let name = format!("{sha256}.{extension}");
        store_image(artifact_dir, &name, &bytes)?;
        *part = json!({
            "type":"text",
            "text":format!("[Saved image artifact: {name}; MIME: {mime}; bytes: {}; SHA256: {sha256}. Use artifact_view with this path when its pixels are needed.]",bytes.len())
        });
    }
    Ok(persisted)
}

/// Read a UTF-8 text range. Offset/nextOffset are byte offsets, never character indices.
pub fn read_artifact(root: &Path, path: &str, offset: usize, limit: usize) -> Result<Value> {
    if limit == 0 {
        bail!("Artifact text limit must be positive");
    }
    let (canonical, relative) = confined_file(root, path)?;
    let bytes = bounded_read(&canonical)?;
    let text = std::str::from_utf8(&bytes)
        .context("Artifact is binary; use the image artifact fetch operation")?;
    if text.contains('\0') {
        bail!("Artifact contains binary NUL bytes; it cannot be read as text");
    }
    if offset > bytes.len() {
        bail!("Artifact offset exceeds total byte length");
    }
    if !text.is_char_boundary(offset) {
        bail!("Artifact offset is inside a UTF-8 character; use the returned nextOffset boundary");
    }
    let mut end = offset
        .saturating_add(limit.min(MAX_ARTIFACT_BYTES))
        .min(bytes.len());
    while end > offset && !text.is_char_boundary(end) {
        end -= 1;
    }
    if limit > 0 && end == offset && offset < bytes.len() {
        bail!("Artifact limit is too small to include the next complete UTF-8 character");
    }
    Ok(json!({
        "path":relative,"sha256":digest(&bytes),"totalBytes":bytes.len(),
        "text":&text[offset..end],"offset":offset,"nextOffset":end,
        "returnedBytes":end-offset,"hasMore":end<bytes.len()
    }))
}

/// Fetch only supported binary images from the canonical artifact root.
pub fn fetch_artifact(root: &Path, path: &str) -> Result<Value> {
    let (canonical, relative) = confined_file(root, path)?;
    let bytes = bounded_read(&canonical)?;
    let mime = image_mime(&bytes).context("Artifact is not a supported PNG or JPEG image")?;
    Ok(
        json!({"path":relative,"mimeType":mime,"base64":STANDARD.encode(&bytes),"sha256":digest(&bytes),"totalBytes":bytes.len()}),
    )
}

fn decode_image_uri(url: &str) -> Result<(&'static str, Vec<u8>)> {
    let (header, payload) = url
        .get(5..)
        .context("Invalid image data URI")?
        .split_once(',')
        .context("Image data URI is missing its payload")?;
    let mime = if header.eq_ignore_ascii_case("image/png;base64") {
        "image/png"
    } else if header.eq_ignore_ascii_case("image/jpeg;base64") {
        "image/jpeg"
    } else {
        bail!("Only base64 PNG and JPEG image data URIs can be stored as artifacts");
    };
    let encoded_limit = MAX_ARTIFACT_BYTES.div_ceil(3) * 4;
    if payload.len() > encoded_limit {
        bail!("Image artifact exceeds the 16 MiB size limit");
    }
    let bytes = STANDARD
        .decode(payload)
        .context("Invalid base64 image payload")?;
    if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
        bail!("Image artifact is empty or exceeds the 16 MiB size limit");
    }
    if image_mime(&bytes) != Some(mime) {
        bail!("Image bytes do not match the PNG/JPEG data URI MIME type");
    }
    Ok((mime, bytes))
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else {
        None
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn confined_file(root: &Path, path: &str) -> Result<(PathBuf, String)> {
    if path.is_empty() || path.contains('\0') {
        bail!("Artifact path is empty or invalid");
    }
    let root = root
        .canonicalize()
        .context("Artifact root does not exist")?;
    if !root.is_dir() {
        bail!("Artifact root must be a directory");
    }
    let requested = Path::new(path);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let canonical = candidate
        .canonicalize()
        .context("Artifact does not exist")?;
    if !canonical.starts_with(&root) || canonical == root {
        bail!("Artifact path escapes the configured artifact root");
    }
    if !canonical.is_file() {
        bail!("Artifact path must refer to a regular file");
    }
    let relative = canonical
        .strip_prefix(&root)?
        .to_string_lossy()
        .into_owned();
    Ok((canonical, relative))
}

fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).context("Could not open artifact")?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("Artifact must be a regular file");
    }
    if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        bail!("Artifact exceeds the 16 MiB size limit");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("Could not read artifact")?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        bail!("Artifact grew beyond the 16 MiB size limit while reading");
    }
    Ok(bytes)
}

fn store_image(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let was_missing = !directory.exists();
    std::fs::create_dir_all(directory).context("Could not create artifact directory")?;
    let root = directory
        .canonicalize()
        .context("Could not resolve artifact directory")?;
    #[cfg(unix)]
    if was_missing {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = was_missing;
    let target = root.join(name);
    if target.try_exists()? {
        return verify_existing(&target, bytes);
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = root.join(format!(
        ".{name}.{}.{nanos}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("Could not create temporary artifact")?;
    let guard = TemporaryArtifact(temporary.clone());
    file.write_all(bytes)
        .context("Could not write image artifact")?;
    file.sync_all()
        .context("Could not persist image artifact")?;
    drop(file);
    if let Err(error) = std::fs::rename(&temporary, &target) {
        if target.try_exists()? {
            verify_existing(&target, bytes)?;
        } else {
            return Err(error).context("Could not publish image artifact");
        }
    }
    drop(guard);
    Ok(())
}

fn verify_existing(path: &Path, expected: &[u8]) -> Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("Artifact destination must not be a symbolic link");
    }
    if bounded_read(path)? != expected {
        bail!("Existing artifact content does not match its SHA256 filename");
    }
    Ok(())
}

struct TemporaryArtifact(PathBuf);
impl Drop for TemporaryArtifact {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "harness-artifacts-{nanos}-{}",
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn png() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\nTEST_FIXTURE_ONLY".to_vec()
    }
    fn image_message(mime: &str, bytes: &[u8]) -> ChatMessage {
        let mut message = ChatMessage::text("tool", "");
        message.content = Some(
            json!([{"type":"text","text":"observed screen"},{"type":"image_url","image_url":{"url":format!("data:{mime};base64,{}",STANDARD.encode(bytes))}}]),
        );
        message
    }

    #[test]
    fn offload_preserves_original_and_exact_bytes_with_content_hash_name() {
        let dir = Directory::new();
        let bytes = png();
        let original = image_message("image/png", &bytes);
        let persisted = offload_message(&original, &dir.0).unwrap();
        assert!(original.content.as_ref().unwrap()[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert_eq!(persisted.content.as_ref().unwrap()[1]["type"], "text");
        assert!(!serde_json::to_string(&persisted)
            .unwrap()
            .contains("base64,"));
        let name = format!("{}.png", digest(&bytes));
        assert_eq!(std::fs::read(dir.0.join(&name)).unwrap(), bytes);
        let fetched = fetch_artifact(&dir.0, &name).unwrap();
        assert_eq!(fetched["mimeType"], "image/png");
        assert_eq!(fetched["base64"], STANDARD.encode(&bytes));
        assert_eq!(fetched["sha256"], digest(&bytes));
        offload_message(&original, &dir.0).unwrap();
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn rejects_unsupported_mime_mismatched_bytes_and_malformed_base64() {
        let dir = Directory::new();
        for mime in ["image/svg+xml", "text/plain", "image/gif", "image/jpeg"] {
            assert!(offload_message(&image_message(mime, &png()), &dir.0).is_err());
        }
        let mut message = image_message("image/png", &png());
        message.content.as_mut().unwrap()[1]["image_url"]["url"] =
            json!("data:image/png;base64,%%%%");
        assert!(offload_message(&message, &dir.0).is_err());
    }

    #[test]
    fn jpeg_is_stored_and_retrieved_with_actual_mime() {
        let dir = Directory::new();
        let bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0xff, 0xd9];
        let message = image_message("image/jpeg", &bytes);
        offload_message(&message, &dir.0).unwrap();
        let result = fetch_artifact(&dir.0, &format!("{}.jpg", digest(&bytes))).unwrap();
        assert_eq!(result["mimeType"], "image/jpeg");
        assert_eq!(result["totalBytes"], bytes.len());
    }

    #[test]
    fn confines_paths_and_rejects_non_image_fetch() {
        let dir = Directory::new();
        let outside = Directory::new();
        std::fs::write(outside.0.join("secret.txt"), b"outside").unwrap();
        assert!(read_artifact(
            &dir.0,
            outside.0.join("secret.txt").to_str().unwrap(),
            0,
            100
        )
        .is_err());
        let traversal = format!(
            "../{}/secret.txt",
            outside.0.file_name().unwrap().to_string_lossy()
        );
        assert!(read_artifact(&dir.0, &traversal, 0, 100).is_err());
        std::fs::write(dir.0.join("text.txt"), b"hello").unwrap();
        assert!(fetch_artifact(&dir.0, "text.txt").is_err());
        assert!(read_artifact(&dir.0, "", 0, 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_cannot_escape_artifact_root() {
        let dir = Directory::new();
        let outside = Directory::new();
        std::fs::write(outside.0.join("secret.txt"), b"outside").unwrap();
        std::os::unix::fs::symlink(outside.0.join("secret.txt"), dir.0.join("escape.txt")).unwrap();
        assert!(read_artifact(&dir.0, "escape.txt", 0, 100).is_err());
        assert!(fetch_artifact(&dir.0, "escape.txt").is_err());
    }

    #[test]
    fn text_slices_preserve_utf8_boundaries_and_pagination() {
        let dir = Directory::new();
        let text = "你好🙂abc";
        std::fs::write(dir.0.join("log.txt"), text).unwrap();
        let first = read_artifact(&dir.0, "log.txt", 0, 7).unwrap();
        assert_eq!(first["text"], "你好");
        assert_eq!(first["nextOffset"], 6);
        assert_eq!(first["hasMore"], true);
        assert_eq!(first["sha256"], digest(text.as_bytes()));
        let second = read_artifact(&dir.0, "log.txt", 6, 100).unwrap();
        assert_eq!(second["text"], "🙂abc");
        assert_eq!(second["hasMore"], false);
        assert_eq!(second["totalBytes"], text.len());
        assert!(read_artifact(&dir.0, "log.txt", 1, 10).is_err());
        assert!(read_artifact(&dir.0, "log.txt", 0, 1).is_err());
        assert!(read_artifact(&dir.0, "log.txt", text.len() + 1, 10).is_err());
    }

    #[test]
    fn rejects_binary_text_and_oversized_artifacts() {
        let dir = Directory::new();
        std::fs::write(dir.0.join("screen.png"), png()).unwrap();
        assert!(read_artifact(&dir.0, "screen.png", 0, 100).is_err());
        std::fs::write(dir.0.join("nul.bin"), b"a\0b").unwrap();
        assert!(read_artifact(&dir.0, "nul.bin", 0, 100).is_err());
        File::create(dir.0.join("large.bin"))
            .unwrap()
            .set_len(MAX_ARTIFACT_BYTES as u64 + 1)
            .unwrap();
        assert!(read_artifact(&dir.0, "large.bin", 0, 1).is_err());
        assert!(fetch_artifact(&dir.0, "large.bin").is_err());
    }

    #[test]
    fn rejects_encoded_images_over_limit_before_decoding_or_disk_write() {
        let payload = "A".repeat(MAX_ARTIFACT_BYTES.div_ceil(3) * 4 + 1);
        let uri = format!("data:image/png;base64,{payload}");
        assert!(decode_image_uri(&uri).is_err());
        assert!(decode_image_uri("data:image/png;charset=utf-8;base64,AA==").is_err());
    }
}
