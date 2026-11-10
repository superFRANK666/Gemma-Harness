//! Bounded GGUF header/metadata inspection. Tensor descriptors and weights are not parsed or hashed.
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

const MAX_HEADER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_METADATA_ENTRIES: u64 = 16_384;
const MAX_CAPTURED_ENTRIES: usize = 2048;
const MAX_TENSORS: u64 = 1_000_000;
const MAX_STRING_BYTES: u64 = 8 * 1024 * 1024;
const MAX_KEY_BYTES: u64 = 1024;
const MAX_DISPLAY_STRING_BYTES: u64 = 4096;
const MAX_ARRAY_ELEMENTS: u64 = 2_000_000;
const MAX_TOTAL_VALUES: u64 = 4_000_000;
const MAX_ARRAY_DEPTH: usize = 8;

/// Inspect the actual selected file without reading or hashing its potentially huge tensor body.
/// A metadata fingerprint is not a weight hash, provenance certificate, or compatibility test.
pub fn inspect_gguf(path: &Path) -> Result<Value> {
    let file =
        File::open(path).with_context(|| format!("Could not open GGUF file {}", path.display()))?;
    let metadata = file
        .metadata()
        .context("Could not read GGUF file metadata")?;
    if !metadata.is_file() {
        bail!("GGUF path must refer to a regular file");
    }
    let mut reader = MetadataReader {
        input: BufReader::with_capacity(16 * 1024, file),
        size: metadata.len(),
        position: 0,
        values: 0,
        fingerprint: Sha256::new(),
    };
    if reader.bytes::<4>()? != *b"GGUF" {
        bail!("Invalid GGUF magic; expected a little-endian GGUF file");
    }
    let version = reader.u32()?;
    if !matches!(version, 2 | 3) {
        bail!(
            "Unsupported GGUF version {version}; only little-endian versions 2 and 3 are supported"
        );
    }
    let tensor_count = reader.u64()?;
    let metadata_count = reader.u64()?;
    if tensor_count > MAX_TENSORS {
        bail!("GGUF tensor count exceeds inspection limit");
    }
    if metadata_count > MAX_METADATA_ENTRIES {
        bail!("GGUF metadata count exceeds inspection limit");
    }
    let mut seen = HashSet::new();
    let mut captured = Map::new();
    let mut contexts = Map::new();
    let mut templates = Map::new();
    for _ in 0..metadata_count {
        let key = reader
            .string(MAX_KEY_BYTES)
            .context("Invalid GGUF metadata key")?;
        if key.is_empty() {
            bail!("GGUF metadata key cannot be empty");
        }
        if !seen.insert(key.clone()) {
            bail!("Duplicate GGUF metadata key: {key}");
        }
        let kind = reader.u32()?;
        ensure_type(kind)?;
        let template_key =
            key == "tokenizer.chat_template" || key.starts_with("tokenizer.chat_template.");
        if relevant_key(&key) && captured.len() >= MAX_CAPTURED_ENTRIES {
            bail!("GGUF relevant metadata exceeds the captured-entry inspection limit");
        }
        if template_key && kind == 8 {
            reader.count_values(1)?;
            let value = reader
                .string(MAX_STRING_BYTES)
                .with_context(|| format!("Invalid GGUF chat template at {key}"))?;
            let template = json!({"status":"present","sha256":format!("{:x}",Sha256::digest(value.as_bytes())),"byteLength":value.len(),"contentOmitted":true,"knownIssues":inspect_template_issues(&value)});
            templates.insert(key.clone(), template.clone());
            captured.insert(key, template);
        } else {
            let value = reader.value(kind, 0, relevant_key(&key))?;
            if let Some(value) = value {
                if key.ends_with(".context_length") && value.as_u64().is_some_and(|v| v > 0) {
                    contexts.insert(key.clone(), value.clone());
                }
                if template_key {
                    templates.insert(
                        key.clone(),
                        json!({"status":"unsupported_type","metadataType":type_name(kind)}),
                    );
                }
                captured.insert(key, value);
            }
        }
    }
    let architecture = captured.get("general.architecture").and_then(Value::as_str);
    let context_key = architecture.map(|name| format!("{name}.context_length"));
    let (context_length, context_source) = if let Some((key, value)) = context_key
        .as_ref()
        .and_then(|key| contexts.get(key).map(|value| (key, value)))
    {
        (value.clone(), json!(key))
    } else if contexts.len() == 1 {
        let (key, value) = contexts.iter().next().unwrap();
        (value.clone(), json!(key))
    } else {
        (Value::Null, Value::Null)
    };
    let header_end = reader.position;
    let header_hash = format!("{:x}", reader.fingerprint.finalize());
    let default_template = templates
        .get("tokenizer.chat_template")
        .cloned()
        .unwrap_or_else(|| json!({"status":"missing","sha256":null}));
    Ok(json!({
        "status":"metadata_inspected",
        "file":{"path":path.to_string_lossy(),"fileName":path.file_name().map(|v|v.to_string_lossy().into_owned()),"sizeBytes":metadata.len()},
        "gguf":{"version":version,"tensorCount":tensor_count,"metadataCount":metadata_count,"metadataEndOffset":header_end},
        "identity":{
            "name":captured.get("general.name"),"architecture":captured.get("general.architecture"),
            "fileType":captured.get("general.file_type"),
            "architectureContextLength":context_length,"contextLengthSource":context_source,
            "contextLengths":contexts,"chatTemplate":default_template,"chatTemplates":templates,
            "metadataFingerprint":{"algorithm":"sha256","sha256":header_hash,"scope":"gguf_header_and_metadata_only","bytesHashed":header_end}
        },
        "metadata":captured,
        "exactModel":{"status":"unknown","revision":"unknown","weightsHashed":false,"certified":false,
            "message":"Metadata declarations and file names do not verify the base weights, repository revision, tensor integrity or runtime compatibility. Tensor descriptors and tensor data were not inspected."},
        "limits":{"maxMetadataBytes":MAX_HEADER_BYTES,"maxStringBytes":MAX_STRING_BYTES,"maxArrayElements":MAX_ARRAY_ELEMENTS,"maxArrayDepth":MAX_ARRAY_DEPTH}
    }))
}

/// Detect a specific legacy Gemma 4 pattern observed to filter real property
/// names like `type` and `description` from tool declarations. This is a
/// source-pattern warning, not proof of the runtime's effective rendered prompt.
pub fn inspect_template_issues(source: &str) -> Vec<Value> {
    if source.contains("macro format_parameters(")
        && source.contains("if key not in standard_keys")
        && source.contains("<|tool_call>")
    {
        vec![
            json!({"code":"gemma4_schema_property_filter","detection":"source_pattern","message":"This legacy Gemma 4 template may omit tool properties named type, description, required, properties or nullable. Check the rendered prompt and consider an explicitly selected compatible template."}),
        ]
    } else {
        Vec::new()
    }
}

fn relevant_key(key: &str) -> bool {
    key.starts_with("general.")
        || key.starts_with("tokenizer.chat_template")
        || key.starts_with("tokenizer.ggml.")
        || key.ends_with(".context_length")
        || key.ends_with(".block_count")
        || key.ends_with(".embedding_length")
        || key.ends_with(".expert_count")
        || key.ends_with(".expert_used_count")
        || key.contains(".attention.head_count")
}

fn ensure_type(kind: u32) -> Result<()> {
    if kind > 12 {
        bail!("Unsupported GGUF metadata value type {kind}");
    }
    Ok(())
}

fn type_name(kind: u32) -> &'static str {
    match kind {
        0 => "uint8",
        1 => "int8",
        2 => "uint16",
        3 => "int16",
        4 => "uint32",
        5 => "int32",
        6 => "float32",
        7 => "bool",
        8 => "string",
        9 => "array",
        10 => "uint64",
        11 => "int64",
        12 => "float64",
        _ => "unknown",
    }
}

struct MetadataReader {
    input: BufReader<File>,
    size: u64,
    position: u64,
    values: u64,
    fingerprint: Sha256,
}

impl MetadataReader {
    fn check_bytes(&self, count: u64) -> Result<()> {
        let end = self
            .position
            .checked_add(count)
            .context("GGUF metadata length overflow")?;
        if end > MAX_HEADER_BYTES {
            bail!("GGUF header/metadata exceeds the 64 MiB inspection limit");
        }
        if end > self.size {
            bail!("Truncated GGUF metadata at byte {}", self.position);
        }
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<()> {
        self.check_bytes(buffer.len() as u64)?;
        self.input.read_exact(buffer).with_context(|| {
            format!(
                "Truncated or unreadable GGUF metadata at byte {}",
                self.position
            )
        })?;
        self.fingerprint.update(&*buffer);
        self.position += buffer.len() as u64;
        Ok(())
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut value = [0u8; N];
        self.read(&mut value)?;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    fn count_values(&mut self, count: u64) -> Result<()> {
        self.values = self
            .values
            .checked_add(count)
            .context("GGUF metadata value count overflow")?;
        if self.values > MAX_TOTAL_VALUES {
            bail!("GGUF metadata exceeds total value-count inspection limit");
        }
        Ok(())
    }

    fn string_length(&mut self, limit: u64) -> Result<u64> {
        let length = self.u64()?;
        if length > limit {
            bail!("GGUF string length {length} exceeds inspection limit {limit}");
        }
        self.check_bytes(length)?;
        Ok(length)
    }

    fn string(&mut self, limit: u64) -> Result<String> {
        let length = self.string_length(limit)?;
        let mut bytes = vec![
            0u8;
            usize::try_from(length)
                .context("GGUF string is too large for this platform")?
        ];
        self.read(&mut bytes)?;
        String::from_utf8(bytes).context("GGUF metadata string is not valid UTF-8")
    }

    /// Streams skipped arrays/strings through a small fixed buffer, retaining only a metadata hash.
    fn skip(&mut self, mut length: u64) -> Result<()> {
        self.check_bytes(length)?;
        let mut scratch = [0u8; 8192];
        while length > 0 {
            let count = length.min(scratch.len() as u64) as usize;
            self.read(&mut scratch[..count])?;
            length -= count as u64;
        }
        Ok(())
    }

    fn value(&mut self, kind: u32, depth: usize, capture: bool) -> Result<Option<Value>> {
        ensure_type(kind)?;
        self.count_values(1)?;
        let value = match kind {
            0 => json!(self.bytes::<1>()?[0]),
            1 => json!(self.bytes::<1>()?[0] as i8),
            2 => json!(u16::from_le_bytes(self.bytes()?)),
            3 => json!(i16::from_le_bytes(self.bytes()?)),
            4 => json!(self.u32()?),
            5 => json!(i32::from_le_bytes(self.bytes()?)),
            6 => float_value(f32::from_le_bytes(self.bytes()?) as f64, "float32"),
            7 => {
                let byte = self.bytes::<1>()?[0];
                if byte > 1 {
                    bail!("Invalid GGUF boolean value {byte}");
                }
                json!(byte == 1)
            }
            8 => {
                let length = self.string_length(MAX_STRING_BYTES)?;
                if !capture {
                    self.skip(length)?;
                    return Ok(None);
                }
                if capture && length <= MAX_DISPLAY_STRING_BYTES {
                    let mut bytes = vec![0u8; length as usize];
                    self.read(&mut bytes)?;
                    json!(String::from_utf8(bytes)
                        .context("GGUF metadata string is not valid UTF-8")?)
                } else {
                    self.skip(length)?;
                    json!({"type":"string","byteLength":length,"contentOmitted":true})
                }
            }
            9 => {
                if depth >= MAX_ARRAY_DEPTH {
                    bail!("GGUF nested metadata arrays exceed depth limit");
                }
                let element_type = self.u32()?;
                ensure_type(element_type)?;
                let length = self.u64()?;
                if length > MAX_ARRAY_ELEMENTS {
                    bail!("GGUF array length {length} exceeds inspection limit");
                }
                match element_type {
                    0 | 1 | 2 | 3 | 4 | 5 | 6 | 10 | 11 | 12 => {
                        self.count_values(length)?;
                        let size = match element_type {
                            0 | 1 => 1,
                            2 | 3 => 2,
                            4..=6 => 4,
                            _ => 8,
                        };
                        self.skip(
                            length
                                .checked_mul(size)
                                .context("GGUF array byte length overflow")?,
                        )?;
                    }
                    _ => {
                        for _ in 0..length {
                            self.value(element_type, depth + 1, false)?;
                        }
                    }
                }
                if !capture {
                    return Ok(None);
                }
                json!({"type":"array","elementType":type_name(element_type),"length":length,"contentOmitted":true})
            }
            10 => json!(self.u64()?),
            11 => json!(i64::from_le_bytes(self.bytes()?)),
            12 => float_value(f64::from_le_bytes(self.bytes()?), "float64"),
            _ => unreachable!(),
        };
        Ok(if capture { Some(value) } else { None })
    }
}

fn float_value(value: f64, kind: &str) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        json!({"type":kind,"nonFinite":if value.is_nan(){"NaN"}else if value.is_sign_positive(){"Infinity"}else{"-Infinity"}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        path: PathBuf,
    }
    impl Fixture {
        fn new(bytes: &[u8]) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "harness-gguf-{time}-{}.gguf",
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = File::create(&path).unwrap();
            file.write_all(bytes).unwrap();
            Self { path }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn string(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&(value.len() as u64).to_le_bytes());
        output.extend_from_slice(value.as_bytes());
    }
    fn header(count: u64) -> Vec<u8> {
        let mut data = b"GGUF".to_vec();
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        data.extend_from_slice(&count.to_le_bytes());
        data
    }
    fn kv_string(output: &mut Vec<u8>, key: &str, value: &str) {
        string(output, key);
        output.extend_from_slice(&8u32.to_le_bytes());
        string(output, value);
    }
    fn kv_u32(output: &mut Vec<u8>, key: &str, value: u32) {
        string(output, key);
        output.extend_from_slice(&4u32.to_le_bytes());
        output.extend_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn captures_identity_and_template_hash_without_certifying_weights() {
        let template = "<bos><|turn>user\n{{ message }}<turn|>\n你好";
        let mut data = header(6);
        kv_string(&mut data, "general.name", "Gemma fixture");
        kv_string(&mut data, "general.architecture", "gemma4");
        kv_u32(&mut data, "general.file_type", 3);
        kv_u32(&mut data, "gemma4.context_length", 262144);
        kv_string(&mut data, "tokenizer.chat_template", template);
        string(&mut data, "tokenizer.ggml.tokens");
        data.extend_from_slice(&9u32.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&3u64.to_le_bytes());
        for token in ["one", "你好", "<|tool_call>"] {
            string(&mut data, token);
        }
        let hash = format!("{:x}", Sha256::digest(&data));
        let end = data.len();
        data.extend_from_slice(b"UNREAD_TENSOR_DATA");
        let fixture = Fixture::new(&data);
        let result = inspect_gguf(&fixture.path).unwrap();
        assert_eq!(result["identity"]["name"], "Gemma fixture");
        assert_eq!(result["identity"]["architectureContextLength"], 262144);
        assert_eq!(
            result["identity"]["chatTemplate"]["sha256"],
            format!("{:x}", Sha256::digest(template.as_bytes()))
        );
        assert_eq!(result["identity"]["metadataFingerprint"]["sha256"], hash);
        assert_eq!(result["gguf"]["metadataEndOffset"], end);
        assert_eq!(result["metadata"]["tokenizer.ggml.tokens"]["length"], 3);
        assert_eq!(result["exactModel"]["status"], "unknown");
        assert_eq!(result["exactModel"]["weightsHashed"], false);
        assert!(!result.to_string().contains("UNREAD_TENSOR_DATA"));
        assert!(!result.to_string().contains(template));
    }

    #[test]
    fn identifies_legacy_gemma4_property_filter_without_exposing_template_text() {
        let old = "{% macro format_parameters(properties, required) %}{% if key not in standard_keys %}<|tool_call>{% endif %}{% endmacro %}";
        let modern = "{% macro format_parameters(properties, required, filter_keys=false) %}{% if not filter_keys or key not in standard_keys %}<|tool_call>{% endif %}{% endmacro %}";
        let mut data = header(2);
        kv_string(&mut data, "general.architecture", "gemma4");
        kv_string(&mut data, "tokenizer.chat_template", old);
        let fixture = Fixture::new(&data);
        let result = inspect_gguf(&fixture.path).unwrap();
        assert_eq!(
            result["identity"]["chatTemplate"]["knownIssues"][0]["code"],
            "gemma4_schema_property_filter"
        );
        assert!(!result.to_string().contains(old));
        assert!(inspect_template_issues(modern).is_empty());
    }

    #[test]
    fn metadata_hash_excludes_even_a_large_sparse_tensor_body() {
        let data = header(0);
        let fixture = Fixture::new(&data);
        File::options()
            .write(true)
            .open(&fixture.path)
            .unwrap()
            .set_len(1024 * 1024 * 1024)
            .unwrap();
        let result = inspect_gguf(&fixture.path).unwrap();
        assert_eq!(result["file"]["sizeBytes"], 1024 * 1024 * 1024u64);
        assert_eq!(result["identity"]["metadataFingerprint"]["bytesHashed"], 24);
        assert_eq!(
            result["identity"]["metadataFingerprint"]["sha256"],
            format!("{:x}", Sha256::digest(data))
        );
    }

    #[test]
    fn rejects_truncated_headers_keys_and_arrays() {
        for bytes in [b"GGU".to_vec(), header(1)] {
            let f = Fixture::new(&bytes);
            assert!(inspect_gguf(&f.path).is_err());
        }
        let mut data = header(1);
        string(&mut data, "tokenizer.ggml.tokens");
        data.extend_from_slice(&9u32.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&100u64.to_le_bytes());
        data.extend_from_slice(b"bad");
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
    }

    #[test]
    fn rejects_invalid_magic_versions_and_missing_paths() {
        let bad = Fixture::new(b"NOT_A_GGUF");
        assert!(inspect_gguf(&bad.path).is_err());
        let mut data = header(0);
        data[4..8].copy_from_slice(&99u32.to_le_bytes());
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
        assert!(inspect_gguf(&f.path.with_extension("missing")).is_err());
    }

    #[test]
    fn rejects_oversized_strings_arrays_and_duplicate_keys() {
        let mut data = header(1);
        data.extend_from_slice(&(MAX_KEY_BYTES + 1).to_le_bytes());
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
        let mut data = header(1);
        string(&mut data, "tokenizer.ggml.tokens");
        data.extend_from_slice(&9u32.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&(MAX_ARRAY_ELEMENTS + 1).to_le_bytes());
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
        let mut data = header(2);
        kv_string(&mut data, "general.name", "A");
        kv_string(&mut data, "general.name", "B");
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
    }

    #[test]
    fn rejects_deep_nested_arrays_unknown_types_and_invalid_bools() {
        let mut data = header(1);
        string(&mut data, "nested");
        data.extend_from_slice(&9u32.to_le_bytes());
        for _ in 0..=MAX_ARRAY_DEPTH {
            data.extend_from_slice(&9u32.to_le_bytes());
            data.extend_from_slice(&1u64.to_le_bytes());
        }
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
        let mut data = header(1);
        string(&mut data, "bad_type");
        data.extend_from_slice(&77u32.to_le_bytes());
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
        let mut data = header(1);
        string(&mut data, "bad_bool");
        data.extend_from_slice(&7u32.to_le_bytes());
        data.push(7);
        let f = Fixture::new(&data);
        assert!(inspect_gguf(&f.path).is_err());
    }

    #[test]
    fn supports_v2_and_does_not_choose_ambiguous_context_metadata() {
        let mut data = header(2);
        data[4..8].copy_from_slice(&2u32.to_le_bytes());
        kv_u32(&mut data, "llama.context_length", 4096);
        kv_u32(&mut data, "gemma4.context_length", 262144);
        let f = Fixture::new(&data);
        let result = inspect_gguf(&f.path).unwrap();
        assert_eq!(result["gguf"]["version"], 2);
        assert!(result["identity"]["architectureContextLength"].is_null());
    }
}
