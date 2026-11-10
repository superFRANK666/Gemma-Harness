//! Workspace tools and process-isolated computer control. No model output is executed
//! until it has passed this module's argument and workspace checks.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

const PREVIEW: usize = 32 * 1024;
const ARTIFACT_LIMIT: usize = 16 * 1024 * 1024;
const FILE_LIMIT: u64 = 2 * 1024 * 1024;
const FILE_WRITE_LIMIT: u64 = 16 * 1024 * 1024;
const SEARCH_FILE_LIMIT: u64 = 64 * 1024 * 1024;
const SEARCH_LINE_LIMIT: u64 = 256 * 1024;
const WORKER_MESSAGE_LIMIT: usize = 32 * 1024 * 1024;
static DESKTOP_LEASE: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

/// A state-changing local helper request was at least partly sent, but no
/// trustworthy result arrived. Callers must inspect state before deliberate
/// retry; cleanup has already run before this error is returned.
#[derive(Debug)]
pub struct IndeterminateToolOperation {
    pub tool: String,
    pub reason: String,
    source: anyhow::Error,
}
impl std::fmt::Display for IndeterminateToolOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter,"Local tool {} has an indeterminate outcome and may already have taken effect; inspect state before retrying: {}",self.tool,self.reason)
    }
}
impl std::error::Error for IndeterminateToolOperation {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub struct Executor {
    workspace: PathBuf,
    artifact_dir: PathBuf,
    browser: Mutex<Option<Worker>>,
    desktop: Mutex<Option<Worker>>,
    desktop_lease: Arc<Mutex<()>>,
    read_versions: std::sync::Mutex<ReadVersions>,
}

#[derive(Default)]
struct ReadVersions {
    entries: HashMap<String, ReadVersion>,
    order: VecDeque<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileStamp {
    fn from_path(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

#[derive(Clone)]
struct ReadVersion {
    path: PathBuf,
    sha256: String,
    stamp: FileStamp,
}

impl Executor {
    pub fn new(workspace: PathBuf, artifact_dir: PathBuf) -> Result<Self> {
        let workspace = workspace.canonicalize().context("Workspace must exist")?;
        if !workspace.is_dir() {
            bail!("Workspace is not a directory");
        }
        std::fs::create_dir_all(&artifact_dir)?;
        let artifact_dir = artifact_dir.canonicalize()?;
        Ok(Self {
            workspace,
            artifact_dir,
            browser: Mutex::new(None),
            desktop: Mutex::new(None),
            desktop_lease: DESKTOP_LEASE
                .get_or_init(|| Arc::new(Mutex::new(())))
                .clone(),
            read_versions: std::sync::Mutex::new(ReadVersions::default()),
        })
    }

    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            bail!("Operation cancelled");
        }
        match name {
            "file_list" => self.file_list(&args, &cancel),
            "file_read" => self.file_read(&args),
            "file_read_chunk" => self.file_read_chunk(&args, &cancel),
            "file_search" => self.file_search(&args, &cancel),
            "file_stat" => self.file_stat(&args, &cancel),
            "file_write" => self.file_write(&args, false),
            "file_apply_patch" | "apply_patch" => self.file_write(&args, true),
            "file_delete" => self.file_delete(&args, &cancel),
            "file_restore" => self.file_restore(&args, &cancel),
            "git_status" | "git_diff" | "terminal_run" => {
                self.execute_process(name, &args, cancel, None).await
            }
            "browser_observe" => self.worker_call(false, "observe", args, cancel).await,
            "browser_screenshot" => self.worker_call(false, "screenshot", args, cancel).await,
            "browser_act" => self.worker_call(false, "act", args, cancel).await,
            "desktop_observe" => self.worker_call(true, "observe", args, cancel).await,
            "desktop_screenshot" => self.worker_call(true, "screenshot", args, cancel).await,
            "desktop_act" => self.worker_call(true, "act", args, cancel).await,
            _ => bail!("Unknown tool: {name}"),
        }
    }

    /// Streams live stdout/stderr for process-backed tools. Other tools retain
    /// the existing execute behavior. Events share one bounded preview budget;
    /// artifacts still retain the original byte streams up to ARTIFACT_LIMIT.
    pub async fn execute_streaming(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
        events: tokio::sync::mpsc::UnboundedSender<Value>,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            bail!("Operation cancelled");
        }
        match name {
            "terminal_run" | "git_status" | "git_diff" => {
                self.execute_process(
                    name,
                    &args,
                    cancel,
                    Some(Arc::new(OutputEvents::new(events))),
                )
                .await
            }
            _ => self.execute(name, args, cancel).await,
        }
    }

    async fn execute_process(
        &self,
        name: &str,
        args: &Value,
        cancel: CancellationToken,
        events: Option<Arc<OutputEvents>>,
    ) -> Result<Value> {
        let (program, arguments, timeout) = match name {
            "git_status" => ("git", vec!["status", "--short", "--branch"], 30_000),
            "git_diff" => (
                "git",
                vec!["diff", "--no-ext-diff", "--no-color", "--", "."],
                30_000,
            ),
            "terminal_run" => {
                let command = string(args, "command")?;
                let timeout = args["timeout_ms"]
                    .as_u64()
                    .unwrap_or(120_000)
                    .clamp(100, 600_000);
                #[cfg(windows)]
                let invocation = (
                    "powershell.exe",
                    vec![
                        "-NoLogo",
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        command,
                    ],
                    timeout,
                );
                #[cfg(not(windows))]
                let invocation = ("/bin/sh", vec!["-c", command], timeout);
                invocation
            }
            _ => bail!("Unknown process tool: {name}"),
        };
        self.run_with_env_events(program, &arguments, timeout, cancel, &[], events)
            .await
    }

    pub async fn capabilities(&self) -> Result<Value> {
        let browser = self.browser_status().await;
        let native = if native_program().is_ok() {
            match self
                .worker_call(true, "capabilities", json!({}), CancellationToken::new())
                .await
            {
                Ok(value) => value,
                Err(error) => json!({"available":false,"error":format!("{error:#}")}),
            }
        } else {
            json!({"available":false,"error":"Native helper is not built for this platform"})
        };
        Ok(
            json!({"workspace": self.workspace, "platform": std::env::consts::OS,
            "file_tools": true, "terminal": true,
            "browser": browser,
            "desktop": {"helper_found": native_program().is_ok(), "requires_os_permission": true,
                "native":native,
                "linux": "X11 or a Wayland desktop exposing RemoteDesktop + ScreenCast portals; AT-SPI is used when available"}}),
        )
    }

    fn browser_cache(&self) -> PathBuf {
        self.artifact_dir
            .parent()
            .unwrap_or(&self.artifact_dir)
            .join("browser-cache")
    }

    /// GUI setup action: uses the app's bundled Node and Playwright resources.
    /// Downloads Chromium into application data, without requiring a user CLI.
    pub async fn setup_browser(&self, cancel: CancellationToken) -> Result<Value> {
        if cancel.is_cancelled() {
            bail!("Browser setup cancelled");
        }
        let cli = resource("browser/node_modules/playwright/cli.js").context(
            "Bundled Playwright resources are missing; reinstall the application package",
        )?;
        let node = browser_node();
        let cache = self.browser_cache();
        std::fs::create_dir_all(&cache)?;
        let result = self
            .run_with_env(
                node.to_str().context("Invalid Node executable path")?,
                &[
                    cli.to_str().context("Invalid Playwright resource path")?,
                    "install",
                    "chromium",
                ],
                600_000,
                cancel,
                &[("PLAYWRIGHT_BROWSERS_PATH", cache.as_os_str())],
            )
            .await?;
        if result["cancelled"] == true {
            return Ok(json!({"cancelled":true,"ready":false,"installation":result}));
        }
        if result["timed_out"] == true || result["exit_code"] != 0 {
            bail!(
                "Browser installation failed: {}",
                result["stderr"]
                    .as_str()
                    .unwrap_or("see installation artifact")
            );
        }
        let browser = self.browser_status().await;
        Ok(json!({"ready":browser["ready"],"browser":browser,"installation":result}))
    }

    async fn browser_status(&self) -> Value {
        let node = browser_node();
        let worker = resource("browser/worker.mjs");
        let package = resource("browser/node_modules/playwright");
        let mut status = json!({"worker_found":worker.is_some(),"node_executable":node,"bundled_node":resource(if cfg!(windows){"browser/bin/node.exe"}else{"browser/bin/node"}).is_some(),"playwright_resources":package.is_some(),"browser_cache":self.browser_cache(),"ready":false});
        let Some(package) = package else {
            status["error"] = json!("Bundled Playwright resources are missing");
            return status;
        };
        let code="const fs=require('fs');const p=require(process.argv[1]).chromium.executablePath();process.stdout.write(JSON.stringify({executable:p,installed:fs.existsSync(p),node:process.version}));";
        let mut command = Command::new(&node);
        command
            .args(["--input-type=commonjs", "-e", code])
            .arg(package)
            .env("PLAYWRIGHT_BROWSERS_PATH", self.browser_cache())
            .kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(5), command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                match serde_json::from_slice::<Value>(&output.stdout) {
                    Ok(result) => {
                        status["ready"] = json!(result["installed"] == true && worker.is_some());
                        status["runtime"] = result;
                    }
                    Err(error) => status["error"] = json!(error.to_string()),
                }
            }
            Ok(Ok(output)) => {
                status["error"] = json!(truncate(&String::from_utf8_lossy(&output.stderr), 1000))
            }
            Ok(Err(error)) => status["error"] = json!(format!("Node runtime unavailable: {error}")),
            Err(_) => status["error"] = json!("Browser readiness probe timed out"),
        }
        status
    }

    fn resolve(&self, requested: &str) -> Result<PathBuf> {
        let path = Path::new(requested);
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        };
        // Canonicalize the nearest existing ancestor, including symlinks, before
        // appending missing path components. This also handles new files safely.
        let mut existing = joined.as_path();
        let mut tail = Vec::new();
        while !existing.exists() {
            let name = existing.file_name().context("Invalid file path")?;
            tail.push(name.to_os_string());
            existing = existing.parent().context("Path has no parent")?;
        }
        let mut resolved = existing.canonicalize()?;
        for part in tail.into_iter().rev() {
            resolved.push(part);
        }
        if !resolved.starts_with(&self.workspace) {
            bail!("Path is outside the selected workspace");
        }
        if resolved
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("Parent traversal is not allowed");
        }
        Ok(resolved)
    }

    fn file_list(&self, args: &Value, cancel: &CancellationToken) -> Result<Value> {
        let root = self.resolve(args["path"].as_str().unwrap_or("."))?;
        let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 1000) as usize;
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let depth = args["depth"].as_u64().unwrap_or(3).clamp(1, 8) as usize;
        let mut entries = Vec::new();
        let mut total = 0;
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .max_depth(depth)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| !excluded(e.path()))
        {
            if cancel.is_cancelled() {
                bail!("Operation cancelled");
            }
            let entry = entry?;
            if entry.depth() == 0 {
                continue;
            }
            if total >= offset && entries.len() < limit {
                entries.push(json!({"path":entry.path().strip_prefix(&self.workspace)?.to_string_lossy(),"directory":entry.file_type().is_dir(),"symlink":entry.file_type().is_symlink()}));
            }
            total += 1;
            if total > offset.saturating_add(limit) {
                break;
            }
        }
        Ok(json!({"entries":entries,"offset":offset,"has_more":total > offset + entries.len()}))
    }

    fn remember_version(&self, path: &Path, sha256: &str) -> Result<String> {
        let read_token = uuid::Uuid::new_v4().to_string();
        let mut versions = self
            .read_versions
            .lock()
            .map_err(|_| anyhow::anyhow!("Read version registry unavailable"))?;
        while versions.order.len() >= 1024 {
            if let Some(old) = versions.order.pop_front() {
                versions.entries.remove(&old);
            }
        }
        versions.entries.insert(
            read_token.clone(),
            ReadVersion {
                path: path.to_path_buf(),
                sha256: sha256.to_owned(),
                stamp: FileStamp::from_path(path)?,
            },
        );
        versions.order.push_back(read_token.clone());
        Ok(read_token)
    }

    fn regular_file_for_removal(&self, requested: &str) -> Result<PathBuf> {
        let original = Path::new(requested);
        let original = if original.is_absolute() {
            original.to_path_buf()
        } else {
            self.workspace.join(original)
        };
        let metadata = std::fs::symlink_metadata(&original).context("File does not exist")?;
        if metadata.file_type().is_symlink() {
            bail!("file_delete and file_stat do not follow a final symbolic link");
        }
        let path = self.resolve(requested)?;
        if path == self.workspace || !std::fs::metadata(&path)?.is_file() {
            bail!("Expected a regular file inside the workspace");
        }
        Ok(path)
    }

    fn file_stat(&self, args: &Value, cancel: &CancellationToken) -> Result<Value> {
        let path = self.regular_file_for_removal(string(args, "path")?)?;
        let before = FileStamp::from_path(&path)?;
        let (sha256, bytes) = hash_file(&path, cancel)?;
        if FileStamp::from_path(&path)? != before || bytes != before.length {
            bail!("File changed during inspection; inspect it again");
        }
        let read_token = self.remember_version(&path, &sha256)?;
        if FileStamp::from_path(&path)? != before {
            bail!("File changed during inspection; inspect it again");
        }
        Ok(
            json!({"path":path.strip_prefix(&self.workspace)?,"sha256":sha256,"read_token":read_token,"bytes":bytes,"kind":"file"}),
        )
    }

    fn file_read_chunk(&self, args: &Value, cancel: &CancellationToken) -> Result<Value> {
        use base64::Engine as _;
        let path = self.resolve(string(args, "path")?)?;
        if !path.is_file() {
            bail!("Expected a regular file inside the workspace");
        }
        let offset = args["offset"].as_u64().unwrap_or(0);
        let limit = args["limit"].as_u64().unwrap_or(6000).clamp(4, 32_000);
        let encoding = args["encoding"].as_str().unwrap_or("utf8");
        if !matches!(encoding, "utf8" | "base64") {
            bail!("encoding must be utf8 or base64");
        }
        let before = FileStamp::from_path(&path)?;
        let previous = if let Some(token) = args.get("read_token") {
            let token = token
                .as_str()
                .context("read_token must be a string")?
                .to_owned();
            let version = self
                .read_versions
                .lock()
                .map_err(|_| anyhow::anyhow!("Read version registry unavailable"))?
                .entries
                .get(&token)
                .cloned()
                .context("Unknown or expired read_token; start again")?;
            if version.path != path || before != version.stamp {
                bail!("File changed or read_token belongs to another file; start again");
            }
            Some((token, version))
        } else {
            None
        };
        if offset > before.length {
            bail!("Offset exceeds file length");
        }
        // Hash and capture the requested bytes in the same pass. Metadata such
        // as modification time can have coarse precision or be restored, so it
        // cannot prove that successive chunks belong to the same contents.
        let (sha256, total, mut bytes) =
            hash_file_with_range(&path, cancel, Some((offset, limit)))?;
        if total != before.length || FileStamp::from_path(&path)? != before {
            bail!("File changed during the read; start again");
        }
        let token = if let Some((token, version)) = previous {
            if version.sha256 != sha256 {
                bail!("File changed since the previous chunk; start again");
            }
            token
        } else {
            self.remember_version(&path, &sha256)?
        };
        if FileStamp::from_path(&path)? != before {
            bail!("File changed during the read; start again");
        }
        let content = if encoding == "base64" {
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        } else {
            let valid = match std::str::from_utf8(&bytes) {
                Ok(_) => bytes.len(),
                Err(error) if error.error_len().is_none() && offset + (bytes.len() as u64) < before.length => error.valid_up_to(),
                Err(_) => bail!("File range is not valid UTF-8 or offset is inside a character; use base64 for binary files"),
            };
            if valid == 0 && !bytes.is_empty() {
                bail!("Limit is too small for the next UTF-8 character");
            }
            bytes.truncate(valid);
            std::str::from_utf8(&bytes)?.to_owned()
        };
        let next = offset + bytes.len() as u64;
        Ok(
            json!({"path":path.strip_prefix(&self.workspace)?,"encoding":encoding,"content":content,"offset":offset,
                  "next_offset":next,"has_more":next < before.length,"file_bytes":before.length,
                  "bytes":bytes.len(),"sha256":sha256,"read_token":token,
                  "instruction":"Offsets are bytes. Continue with next_offset and the same read_token."}),
        )
    }

    fn file_read(&self, args: &Value) -> Result<Value> {
        let path = self.resolve(string(args, "path")?)?;
        let before = FileStamp::from_path(&path)?;
        let bytes = read_small(&path)?;
        if FileStamp::from_path(&path)? != before || bytes.len() as u64 != before.length {
            bail!("File changed during the read; read it again");
        }
        let text = std::str::from_utf8(&bytes).context("File is not UTF-8 text")?;
        let sha256 = hash(&bytes);
        let read_token = self.remember_version(&path, &sha256)?;
        if FileStamp::from_path(&path)? != before {
            bail!("File changed during the read; read it again");
        }
        if args["raw"].as_bool().unwrap_or(false) {
            return Ok(
                json!({"path":path.strip_prefix(&self.workspace)?,"sha256":sha256,"read_token":read_token,"content":text,"content_format":"verbatim_utf8","has_more":false,"raw":true,"bytes":bytes.len(),"instruction":"The content field is the exact file text. If asked to return only the file contents, return the content value alone without JSON metadata or a code fence; preserve whitespace."}),
            );
        }
        let start = args["start_line"].as_u64().unwrap_or(1).max(1) as usize;
        let count = args["line_count"].as_u64().unwrap_or(200).clamp(1, 1000) as usize;
        let lines: Vec<&str> = text.lines().collect();
        let selected: String = lines
            .iter()
            .enumerate()
            .skip(start - 1)
            .take(count)
            .map(|(i, line)| format!("{}: {}\n", i + 1, line))
            .collect();
        let truncated = selected.len() > PREVIEW;
        let full_text_artifact = if truncated {
            let artifact = self
                .artifact_dir
                .join(format!("file-read-{}.txt", uuid::Uuid::new_v4()));
            std::fs::write(&artifact, selected.as_bytes())
                .context("Could not save the complete file-read range")?;
            Some(artifact)
        } else {
            None
        };
        let more_lines = start.saturating_sub(1) + count < lines.len();
        Ok(
            json!({"path":path.strip_prefix(&self.workspace)?,"sha256":sha256,"read_token":read_token,"total_lines":lines.len(),"start_line":start,"text":truncate(&selected,PREVIEW),"content_format":"numbered_display_not_verbatim","instruction":"The text field has line-number prefixes and normalized line endings. For exact file contents call file_read with raw:true.","text_truncated":truncated,"full_text_artifact":full_text_artifact,"has_more":truncated || more_lines,"has_more_lines":more_lines,"next_start_line":if more_lines {Some(start.saturating_add(count))} else {None}}),
        )
    }

    fn file_delete(&self, args: &Value, cancel: &CancellationToken) -> Result<Value> {
        let path = self.regular_file_for_removal(string(args, "path")?)?;
        let (expected, token_stamp) = if let Some(token) = args.get("read_token") {
            if args.get("expected_sha256").is_some() {
                bail!("Provide either read_token or expected_sha256, not both");
            }
            let token = token.as_str().context("read_token must be a string")?;
            let versions = self
                .read_versions
                .lock()
                .map_err(|_| anyhow::anyhow!("Read version registry unavailable"))?;
            let version = versions
                .entries
                .get(token)
                .context("Unknown or expired read_token; inspect the file again")?;
            if version.path != path || FileStamp::from_path(&path)? != version.stamp {
                bail!("File changed or read_token belongs to a different file");
            }
            (version.sha256.clone(), Some(version.stamp.clone()))
        } else {
            (string(args, "expected_sha256")?.to_owned(), None)
        };
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("A valid SHA-256 or read_token is required to delete a file");
        }
        let backup = self
            .artifact_dir
            .join(format!("deleted-{}.bin", uuid::Uuid::new_v4()));
        let manifest = backup.with_extension("json");
        let mut backup_created = false;
        let mut manifest_created = false;
        let attempt = (|| -> Result<(String, u64)> {
            use std::io::{Read, Write};
            let mut source = open_regular_file(&path)?;
            let source_permissions = source.metadata()?.permissions();
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut target = options.open(&backup)?;
            backup_created = true;
            let mut digest = Sha256::new();
            let mut buffer = [0u8; 128 * 1024];
            let mut bytes = 0u64;
            loop {
                if cancel.is_cancelled() {
                    bail!("Operation cancelled before deletion");
                }
                let count = source.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                target.write_all(&buffer[..count])?;
                digest.update(&buffer[..count]);
                bytes = bytes.saturating_add(count as u64);
            }
            target.sync_all()?;
            drop(target);
            drop(source);
            let actual = format!("{:x}", digest.finalize());
            if !actual.eq_ignore_ascii_case(&expected) {
                bail!("File changed since inspection; inspect it again before deletion");
            }
            let metadata =
                json!({"version":1,"sha256":actual,"readonly":source_permissions.readonly()});
            #[cfg(unix)]
            let metadata = {
                use std::os::unix::fs::PermissionsExt;
                let mut metadata = metadata;
                metadata["unix_mode"] = json!(source_permissions.mode());
                metadata
            };
            let mut manifest_options = std::fs::OpenOptions::new();
            manifest_options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                manifest_options.mode(0o600);
            }
            let mut metadata_file = manifest_options.open(&manifest)?;
            manifest_created = true;
            serde_json::to_writer(&mut metadata_file, &metadata)?;
            metadata_file.sync_all()?;
            if hash_file(&path, cancel)?.0 != actual {
                bail!("File changed while deletion was being prepared; inspect it again");
            }
            if let Some(stamp) = &token_stamp {
                if FileStamp::from_path(&path)? != *stamp {
                    bail!(
                        "File identity changed while deletion was being prepared; inspect it again"
                    );
                }
            }
            if cancel.is_cancelled() {
                bail!("Operation cancelled before deletion");
            }
            std::fs::remove_file(&path).context("Could not delete the file")?;
            Ok((actual, bytes))
        })();
        match attempt {
            Ok((sha256, bytes)) => Ok(
                json!({"path":path.strip_prefix(&self.workspace)?,"deleted":true,"sha256":sha256,"bytes":bytes,"backup_artifact":backup}),
            ),
            Err(error) => {
                if backup_created {
                    let _ = std::fs::remove_file(&backup);
                }
                if manifest_created {
                    let _ = std::fs::remove_file(&manifest);
                }
                Err(error)
            }
        }
    }

    fn file_restore(&self, args: &Value, cancel: &CancellationToken) -> Result<Value> {
        let requested_backup = Path::new(string(args, "backup_artifact")?);
        let requested_backup = if requested_backup.is_absolute() {
            requested_backup.to_path_buf()
        } else {
            self.artifact_dir.join(requested_backup)
        };
        if std::fs::symlink_metadata(&requested_backup)?
            .file_type()
            .is_symlink()
        {
            bail!("Backup artifact must not be a symbolic link");
        }
        let backup = requested_backup.canonicalize()?;
        if backup.parent() != Some(self.artifact_dir.as_path())
            || !backup.is_file()
            || !backup
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("deleted-") && n.ends_with(".bin"))
        {
            bail!("Only a deleted-file backup from this artifact store can be restored");
        }
        let expected = string(args, "expected_sha256")?;
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("The SHA-256 returned by file_delete is required");
        }
        let manifest = backup.with_extension("json");
        if std::fs::symlink_metadata(&manifest)?
            .file_type()
            .is_symlink()
        {
            bail!("Backup metadata must not be a symbolic link");
        }
        use std::io::Read;
        let mut metadata_bytes = Vec::new();
        std::fs::File::open(&manifest)?
            .take(4097)
            .read_to_end(&mut metadata_bytes)?;
        if metadata_bytes.len() > 4096 {
            bail!("Backup metadata is too large");
        }
        let metadata: Value = serde_json::from_slice(&metadata_bytes)?;
        if metadata["version"] != 1
            || metadata["sha256"]
                .as_str()
                .is_none_or(|h| !h.eq_ignore_ascii_case(expected))
        {
            bail!("Backup metadata does not match its expected SHA-256");
        }
        let path = self.resolve(string(args, "path")?)?;
        if path == self.workspace {
            bail!("Cannot replace workspace root");
        }
        let parent = path.parent().context("Invalid file parent")?;
        std::fs::create_dir_all(parent)?;
        if !parent.canonicalize()?.starts_with(&self.workspace) {
            bail!("Parent escaped workspace");
        }
        let temp = parent.join(format!(".harness-restore-{}.tmp", uuid::Uuid::new_v4()));
        let mut temp_created = false;
        let attempt = (|| -> Result<(String, u64)> {
            use std::io::{Read, Write};
            let mut source = open_regular_file(&backup)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut target = options.open(&temp)?;
            temp_created = true;
            let mut digest = Sha256::new();
            let mut bytes = 0u64;
            let mut buffer = [0u8; 128 * 1024];
            loop {
                if cancel.is_cancelled() {
                    bail!("Operation cancelled before restore");
                }
                let count = source.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                target.write_all(&buffer[..count])?;
                digest.update(&buffer[..count]);
                bytes = bytes.saturating_add(count as u64);
            }
            target.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata["unix_mode"]
                    .as_u64()
                    .context("Missing original file permissions")?
                    as u32;
                target.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))?;
            }
            #[cfg(not(unix))]
            {
                let mut permissions = target.metadata()?.permissions();
                permissions.set_readonly(
                    metadata["readonly"]
                        .as_bool()
                        .context("Missing original file permissions")?,
                );
                target.set_permissions(permissions)?;
            }
            drop(target);
            drop(source);
            let actual = format!("{:x}", digest.finalize());
            if !actual.eq_ignore_ascii_case(expected) {
                bail!("Backup content does not match its expected SHA-256");
            }
            if cancel.is_cancelled() {
                bail!("Operation cancelled before restore");
            }
            std::fs::hard_link(&temp, &path).context(
                "Restore target already exists or cannot be created without overwriting",
            )?;
            Ok((actual, bytes))
        })();
        let cleanup_warning = if temp_created {
            std::fs::remove_file(&temp).err().map(|e| e.to_string())
        } else {
            None
        };
        match attempt {
            Ok((sha256, bytes)) => Ok(
                json!({"path":path.strip_prefix(&self.workspace)?,"restored":true,"sha256":sha256,"bytes":bytes,"cleanup_warning":cleanup_warning}),
            ),
            Err(error) => Err(error),
        }
    }

    fn file_search(&self, args: &Value, cancel: &CancellationToken) -> Result<Value> {
        let query = string(args, "query")?;
        if query.is_empty() {
            bail!("Search query is empty");
        }
        let root = self.resolve(args["path"].as_str().unwrap_or("."))?;
        let limit = args["limit"].as_u64().unwrap_or(100).clamp(1, 500) as usize;
        let glob = args["glob"]
            .as_str()
            .map(|g| globset::Glob::new(g).map(|g| g.compile_matcher()))
            .transpose()?;
        let mut matches = Vec::new();
        let mut scanned = 0usize;
        let mut searched = 0usize;
        let mut skipped = 0usize;
        let mut skipped_details = Vec::new();
        let mut limit_reached = false;
        let mut scan_limit_reached = false;
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !excluded(e.path()))
        {
            if cancel.is_cancelled() {
                bail!("Operation cancelled");
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    skipped += 1;
                    if skipped_details.len() < 20 {
                        let path = error
                            .path()
                            .and_then(|p| p.strip_prefix(&self.workspace).ok());
                        skipped_details.push(
                            json!({"path":path,"reason":"walk_error","message":error.to_string()}),
                        );
                    }
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(&self.workspace)?;
            if glob.as_ref().is_some_and(|g| !g.is_match(relative)) {
                continue;
            }
            if scanned >= 20_000 {
                scan_limit_reached = true;
                break;
            }
            scanned += 1;
            let bytes = match read_small(entry.path()) {
                Ok(b) => b,
                Err(error) => {
                    if entry.metadata().is_ok_and(|m| m.len() > FILE_LIMIT) {
                        let streamed = search_large_text(
                            entry.path(),
                            relative,
                            query,
                            limit.saturating_sub(matches.len()),
                            cancel,
                        );
                        if cancel.is_cancelled() {
                            bail!("Operation cancelled");
                        }
                        match streamed {
                            Ok((found, reached, incomplete)) => {
                                matches.extend(found);
                                if let Some(reason) = incomplete {
                                    skipped += 1;
                                    if skipped_details.len() < 20 {
                                        skipped_details
                                            .push(json!({"path":relative,"reason":reason}));
                                    }
                                } else {
                                    searched += 1;
                                }
                                if reached {
                                    limit_reached = true;
                                    break;
                                }
                                continue;
                            }
                            Err(stream_error) => {
                                skipped += 1;
                                if skipped_details.len() < 20 {
                                    skipped_details.push(json!({"path":relative,"reason":"read_error","message":stream_error.to_string()}));
                                }
                                continue;
                            }
                        }
                    }
                    skipped += 1;
                    if skipped_details.len() < 20 {
                        skipped_details.push(
                            json!({"path":relative,"reason":"read_error","message":error.to_string()}),
                        );
                    }
                    continue;
                }
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                skipped += 1;
                if skipped_details.len() < 20 {
                    skipped_details.push(json!({"path":relative,"reason":"not_utf8"}));
                }
                continue;
            };
            searched += 1;
            for (i, line) in text.lines().enumerate() {
                if line.contains(query) {
                    matches.push(json!({"path":relative,"line":i+1,"text":truncate(line,500)}));
                    if matches.len() >= limit {
                        limit_reached = true;
                        break;
                    }
                }
            }
            if limit_reached {
                break;
            }
        }
        let complete = skipped == 0 && !limit_reached && !scan_limit_reached;
        Ok(
            json!({"matches":matches,"truncated":!complete,"complete":complete,
            "files_scanned":scanned,"files_searched":searched,"files_skipped":skipped,
            "skipped_details":skipped_details,"skipped_details_omitted":skipped.saturating_sub(20),
            "match_limit_reached":limit_reached,"scan_limit_reached":scan_limit_reached,
            "scope":"Excludes .git, node_modules, target, .venv and dist; does not follow symlinks.",
            "instruction":if complete {"Search completed within the stated scope."} else {"Search is incomplete; do not infer that absent matches do not exist. Inspect skipped paths or narrow the search."}}),
        )
    }

    fn file_write(&self, args: &Value, patch: bool) -> Result<Value> {
        let path = self.resolve(string(args, "path")?)?;
        if path == self.workspace {
            bail!("Cannot replace workspace root");
        }
        let before = if path.exists() {
            Some(read_bounded_file(&path, FILE_WRITE_LIMIT)?)
        } else {
            None
        };
        let token_hash = if let Some(token) = args.get("read_token") {
            if args.get("expected_sha256").is_some() {
                bail!("Provide either read_token or expected_sha256, not both");
            }
            let token = token.as_str().context("read_token must be a string")?;
            let versions = self
                .read_versions
                .lock()
                .map_err(|_| anyhow::anyhow!("Read version registry unavailable"))?;
            let version = versions
                .entries
                .get(token)
                .context("Unknown or expired read_token; read the file again")?;
            if version.path != path || FileStamp::from_path(&path)? != version.stamp {
                bail!("File changed or read_token belongs to a different file");
            }
            Some(version.sha256.clone())
        } else {
            None
        };
        let expected = if let Some(hash) = &token_hash {
            json!(hash)
        } else if let Some(expected) = args.get("expected_sha256") {
            expected.clone()
        } else if !patch {
            // Omitting a version is create-only, never permission to replace.
            Value::Null
        } else {
            bail!("read_token or expected_sha256 is required to patch an existing file; read it first")
        };
        match (&before, expected.as_str()) {
            (Some(bytes), Some(expected)) if hash(bytes) == expected => {}
            (None, None) if expected.is_null() => {}
            (Some(_), _) => {
                bail!("File changed or expected_sha256 missing; read the file again before editing")
            }
            (None, _) => {
                bail!("File does not exist; creating a file requires expected_sha256:null")
            }
        }
        let content = if patch {
            let before =
                std::str::from_utf8(before.as_deref().context("Patch target does not exist")?)?;
            let old = string(args, "old_text")?;
            let replacement = string(args, "new_text")?;
            if old.is_empty() || before.match_indices(old).count() != 1 {
                bail!("old_text must match exactly once");
            }
            before.replacen(old, replacement, 1).into_bytes()
        } else {
            let content = string(args, "content")?;
            match args["encoding"].as_str().unwrap_or("utf8") {
                "utf8" => content.as_bytes().to_vec(),
                "base64" => {
                    use base64::Engine as _;
                    if content.len() > (FILE_WRITE_LIMIT as usize).div_ceil(3) * 4 {
                        bail!("Encoded content exceeds the 16 MiB file limit");
                    }
                    base64::engine::general_purpose::STANDARD
                        .decode(content)
                        .context("Invalid base64 content")?
                }
                _ => bail!("encoding must be utf8 or base64"),
            }
        };
        if content.len() as u64 > FILE_WRITE_LIMIT {
            bail!("File content exceeds 16 MiB");
        }
        // Keep an exact pre-edit artifact so a GUI can offer an explicit undo.
        let backup = if let Some(bytes) = &before {
            let backup = self
                .artifact_dir
                .join(format!("before-{}.bin", uuid::Uuid::new_v4()));
            std::fs::write(&backup, bytes)?;
            Some(backup)
        } else {
            None
        };
        let parent = path.parent().context("Invalid file parent")?;
        std::fs::create_dir_all(parent)?;
        // Recheck containment after directory creation and immediately before writing.
        if !parent.canonicalize()?.starts_with(&self.workspace) {
            bail!("Parent escaped workspace");
        }
        let temp = parent.join(format!(".harness-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        use std::io::Write;
        let mut file = options.open(&temp)?;
        if let Ok(meta) = path.metadata() {
            file.set_permissions(meta.permissions())?;
        }
        file.write_all(&content)?;
        file.sync_all()?;
        drop(file);
        if let Some(bytes) = &before {
            if read_bounded_file(&path, FILE_WRITE_LIMIT)
                .map(|current| hash(&current) != hash(bytes))
                .unwrap_or(true)
            {
                let _ = std::fs::remove_file(&temp);
                bail!("File changed while edit was being prepared; read and retry");
            }
        } else if path.exists() {
            let _ = std::fs::remove_file(&temp);
            bail!("File appeared while edit was being prepared; read and retry");
        }
        let cleanup_warning = if before.is_none() {
            // Unlike rename, hard_link fails atomically if a destination
            // appears after the existence check; never clobber a new file.
            if let Err(err) = std::fs::hard_link(&temp, &path) {
                let _ = std::fs::remove_file(&temp);
                return Err(err)
                    .context("Could not create file without overwriting an existing destination");
            }
            std::fs::remove_file(&temp)
                .err()
                .map(|error| error.to_string())
        } else {
            if let Err(err) = std::fs::rename(&temp, &path) {
                let _ = std::fs::remove_file(&temp);
                return Err(err.into());
            }
            None
        };
        Ok(
            json!({"path":path.strip_prefix(&self.workspace)?,"sha256":hash(&content),"bytes":content.len(),"backup_artifact":backup,"cleanup_warning":cleanup_warning}),
        )
    }

    async fn run_with_env(
        &self,
        program: &str,
        args: &[&str],
        timeout: u64,
        cancel: CancellationToken,
        environment: &[(&str, &std::ffi::OsStr)],
    ) -> Result<Value> {
        self.run_with_env_events(program, args, timeout, cancel, environment, None)
            .await
    }

    async fn run_with_env_events(
        &self,
        program: &str,
        args: &[&str],
        timeout: u64,
        cancel: CancellationToken,
        environment: &[(&str, &std::ffi::OsStr)],
        events: Option<Arc<OutputEvents>>,
    ) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let stdout_path = self.artifact_dir.join(format!("{id}-stdout.txt"));
        let stderr_path = self.artifact_dir.join(format!("{id}-stderr.txt"));
        let mut command = Command::new(program);
        // Windows canonicalization produces verbatim paths. PowerShell's
        // filesystem provider cannot reliably resolve relative paths from
        // those directories, so use the equivalent ordinary path when safe.
        #[cfg(windows)]
        let process_workspace = {
            let path = dunce::simplified(&self.workspace);
            if program.eq_ignore_ascii_case("powershell.exe")
                && path.as_os_str().to_string_lossy().starts_with(r"\\?\")
            {
                bail!("PowerShell cannot safely use this workspace path; choose a local path that does not require a Windows verbatim path");
            }
            path
        };
        #[cfg(not(windows))]
        let process_workspace = self.workspace.as_path();
        command
            .args(args)
            .current_dir(process_workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in environment {
            command.env(name, value);
        }
        configure_group(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("Cannot start {program}"))?;
        let process_id = child.id();
        let out = child.stdout.take().context("Missing stdout")?;
        let err = child.stderr.take().context("Missing stderr")?;
        let out_task = tokio::spawn(capture(out, stdout_path.clone(), "stdout", events.clone()));
        let err_task = tokio::spawn(capture(err, stderr_path.clone(), "stderr", events));
        let mut cancelled = false;
        let mut timed_out = false;
        let status = tokio::select! {
            status=child.wait()=>status,
            _=cancel.cancelled()=>{cancelled=true;kill_group(&mut child).await;child.wait().await},
            _=tokio::time::sleep(Duration::from_millis(timeout))=>{timed_out=true;kill_group(&mut child).await;child.wait().await}
        };
        if status.is_err() {
            kill_group(&mut child).await;
            let _ = child.wait().await;
        }
        // A background descendant may retain pipes after its parent exits. Close
        // the command's process group before waiting for stream completion.
        if let Some(pid) = process_id {
            kill_pid_group(pid, false);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let (stdout, stderr) = tokio::join!(
            settle_capture(out_task, "stdout", deadline),
            settle_capture(err_task, "stderr", deadline)
        );
        let status =
            status.context("Could not wait for command; inspect actual state before retrying")?;
        let output_complete = stdout.error.is_none()
            && stderr.error.is_none()
            && !stdout.truncated
            && !stderr.truncated;
        Ok(
            json!({"exit_code":status.code(),"cancelled":cancelled,"timed_out":timed_out,"stdout":stdout.text,"stderr":stderr.text,"stdout_artifact":stdout_path,"stderr_artifact":stderr_path,"output_complete":output_complete,"isError":!output_complete,"stdout_error":stdout.error,"stderr_error":stderr.error,"stdout_bytes":stdout.received,"stderr_bytes":stderr.received,"stdout_saved_bytes":stdout.saved,"stderr_saved_bytes":stderr.saved,"stdout_artifact_truncated":stdout.truncated,"stderr_artifact_truncated":stderr.truncated}),
        )
    }

    async fn worker_call(
        &self,
        native: bool,
        method: &str,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let _lease = if native {
            Some(
                tokio::select! {v=self.desktop_lease.lock()=>v,_=cancel.cancelled()=>bail!("Operation cancelled")},
            )
        } else {
            None
        };
        let slot = if native { &self.desktop } else { &self.browser };
        let mut guard =
            tokio::select! {v=slot.lock()=>v,_=cancel.cancelled()=>bail!("Operation cancelled")};
        if guard.is_none() {
            *guard = Some(Worker::spawn(native, &self.artifact_dir, &self.workspace).await?);
        }
        let worker = guard.as_mut().unwrap();
        let side_effecting = matches!(method, "act" | "navigate" | "launch")
            || (!native
                && method == "observe"
                && args["url"].as_str().is_some_and(|url| !url.is_empty()));
        let outcome = worker
            .exchange(method, args, &cancel, Duration::from_secs(90))
            .await;
        match outcome {
            Ok(response) => {
                // A correctly framed tool rejection (for example a stale ref)
                // does not corrupt the transport or require closing its browser.
                if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
                    bail!(
                        "{}",
                        error["message"].as_str().unwrap_or("Computer Use failed")
                    );
                }
                Ok(response.get("result").cloned().unwrap_or(Value::Null))
            }
            Err(error) => {
                let indeterminate = side_effecting
                    && error
                        .downcast_ref::<WorkerRequestState>()
                        .is_some_and(|state| state.started);
                if !native && error.is::<WorkerInterruption>() {
                    // Playwright closes its Chromium connection and releases the
                    // persistent profile before forced termination. An in-flight
                    // action may emit an error before the cancel acknowledgement.
                    let _ = tokio::time::timeout(Duration::from_secs(2), async {
                        worker
                            .stdin
                            .write_all(b"{\"id\":\"cancel\",\"method\":\"cancel\",\"params\":{}}\n")
                            .await?;
                        worker.stdin.flush().await?;
                        loop {
                            let line =
                                read_worker_line(&mut worker.stdout, WORKER_MESSAGE_LIMIT).await?;
                            if serde_json::from_slice::<Value>(&line)
                                .ok()
                                .is_some_and(|v| v["id"] == "cancel")
                            {
                                break;
                            }
                        }
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;
                }
                kill_group(&mut worker.child).await;
                let _ = worker.child.wait().await;
                *guard = None;
                if indeterminate {
                    Err(IndeterminateToolOperation {
                        tool: format!("{}_{}", if native { "desktop" } else { "browser" }, method),
                        reason: format!("{error:#}"),
                        source: error,
                    }
                    .into())
                } else {
                    Err(error)
                }
            }
        }
    }
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}
impl Worker {
    async fn exchange(
        &mut self,
        method: &str,
        args: Value,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<Value> {
        let request = json!({"id":uuid::Uuid::new_v4().to_string(),"method":method,"params":args});
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        if bytes.len() > WORKER_MESSAGE_LIMIT {
            bail!("Worker request exceeds 32 MiB");
        }
        let mut request_started = false;
        let outcome = {
            let exchange = async {
                let mut written = 0;
                while written < bytes.len() {
                    let count = self
                        .stdin
                        .write(&bytes[written..])
                        .await
                        .context("Cannot write to Computer Use worker")?;
                    if count == 0 {
                        bail!("Computer Use worker closed its request pipe");
                    }
                    request_started = true;
                    written += count;
                }
                self.stdin
                    .flush()
                    .await
                    .context("Cannot flush Computer Use request")?;
                let line = read_worker_line(&mut self.stdout, WORKER_MESSAGE_LIMIT).await?;
                let response: Value =
                    serde_json::from_slice(&line).context("Invalid worker response")?;
                if response["id"] != request["id"] {
                    bail!("Worker response ID mismatch");
                }
                if !response.is_object()
                    || (!response.get("error").is_some_and(|error| !error.is_null())
                        && response.get("result").is_none())
                {
                    bail!("Worker response must contain result or error");
                }
                Ok(response)
            };
            tokio::select! {
                biased;
                _=cancel.cancelled()=>Err(WorkerInterruption::Cancelled.into()),
                result=tokio::time::timeout(timeout,exchange)=>match result {
                    Ok(result)=>result,
                    Err(_)=>Err(WorkerInterruption::TimedOut.into()),
                }
            }
        };
        outcome.map_err(|error: anyhow::Error| {
            error.context(WorkerRequestState {
                started: request_started,
            })
        })
    }

    async fn spawn(native: bool, artifacts: &Path, workspace: &Path) -> Result<Self> {
        let (mut command, description) = if native {
            let (program, args) = native_program()?;
            let mut c = Command::new(&program);
            c.args(args);
            (c, "Native desktop helper")
        } else {
            let script = resource("browser/worker.mjs").context("Browser worker missing")?;
            let mut c = Command::new(browser_node());
            c.arg(script);
            (c, "Playwright browser worker")
        };
        command
            .env("HARNESS_ARTIFACT_DIR", artifacts)
            .env("HARNESS_WORKSPACE", workspace)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if !native {
            let app_data = artifacts.parent().unwrap_or(artifacts);
            command.env("PLAYWRIGHT_BROWSERS_PATH", app_data.join("browser-cache"));
            if std::env::var_os("HARNESS_BROWSER_PROFILE").is_none() {
                command.env(
                    "HARNESS_BROWSER_PROFILE",
                    app_data
                        .join("browser-profiles")
                        .join(hash(workspace.to_string_lossy().as_bytes())),
                );
            }
        }
        configure_group(&mut command);
        let mut child = command.spawn().with_context(|| {
            format!("{description} cannot start. Check installed runtime and helper build.")
        })?;
        Ok(Self {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            kill_pid_group(pid, true);
        }
        let _ = self.child.start_kill();
    }
}

#[derive(Debug)]
enum WorkerInterruption {
    Cancelled,
    TimedOut,
}
impl std::fmt::Display for WorkerInterruption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Cancelled => "Operation cancelled",
            Self::TimedOut => "Computer Use worker timed out",
        })
    }
}
impl std::error::Error for WorkerInterruption {}

#[derive(Debug)]
struct WorkerRequestState {
    started: bool,
}
impl std::fmt::Display for WorkerRequestState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.started {
            "Computer Use exchange failed after request transmission began"
        } else {
            "Computer Use exchange failed before request transmission"
        })
    }
}

/// Bound allocation while framing, rather than after read_line has accumulated
/// an arbitrarily large response. A partial EOF is never a complete JSONL frame.
async fn read_worker_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut line = Vec::with_capacity(limit.min(8192));
    loop {
        let available = reader
            .fill_buf()
            .await
            .context("Cannot read Computer Use worker response")?;
        if available.is_empty() {
            bail!("Computer Use worker exited before a complete newline-terminated response");
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(count) > limit {
            bail!("Worker response exceeds {limit} byte limit");
        }
        line.extend_from_slice(&available[..count]);
        let complete = available[count - 1] == b'\n';
        reader.consume(count);
        if complete {
            return Ok(line);
        }
    }
}

fn resource(relative: &str) -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = std::env::var_os("HARNESS_RESOURCE_DIR") {
        roots.push(PathBuf::from(root));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            roots.push(p.join("workers"));
            roots.push(p.join("../Resources/workers"));
            roots.push(p.to_path_buf());
        }
    }
    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workers"));
    roots
        .into_iter()
        .flat_map(|p| [p.join(relative), p.join("workers").join(relative)])
        .find(|p| p.exists())
}
fn browser_node() -> PathBuf {
    if let Some(node) = std::env::var_os("HARNESS_NODE") {
        return node.into();
    }
    resource(if cfg!(windows) {
        "browser/bin/node.exe"
    } else {
        "browser/bin/node"
    })
    .unwrap_or_else(|| PathBuf::from("node"))
}
fn native_program() -> Result<(PathBuf, Vec<String>)> {
    if let Some(program) = std::env::var_os("HARNESS_DESKTOP_HELPER") {
        return Ok((program.into(), vec![]));
    }
    let platform = std::env::consts::OS;
    let executable = if cfg!(windows) {
        "harness-desktop.exe"
    } else {
        "harness-desktop"
    };
    if let Some(program) = resource(&format!("native/bin/{platform}/{executable}")) {
        return Ok((program, vec![]));
    }
    #[cfg(target_os = "macos")]
    if let Some(script) = resource("native/macos/DesktopHelper.swift") {
        return Ok((
            PathBuf::from("/usr/bin/swift"),
            vec![script.to_string_lossy().into_owned()],
        ));
    }
    if let Some(program) = resource(&format!("native/target/release/{executable}")) {
        return Ok((program, vec![]));
    }
    bail!("Native helper is not built. Build workers/native for this platform, or set HARNESS_DESKTOP_HELPER.")
}
fn excluded(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| [".git", "node_modules", "target", ".venv", "dist"].contains(&s))
}
fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .with_context(|| format!("{key} must be a string"))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn open_regular_file(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("Expected a regular file");
    }
    Ok(file)
}
fn hash_file(path: &Path, cancel: &CancellationToken) -> Result<(String, u64)> {
    let (sha256, bytes, _) = hash_file_with_range(path, cancel, None)?;
    Ok((sha256, bytes))
}

fn hash_file_with_range(
    path: &Path,
    cancel: &CancellationToken,
    range: Option<(u64, u64)>,
) -> Result<(String, u64, Vec<u8>)> {
    use std::io::Read;
    let mut file = open_regular_file(path)?;
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut captured = Vec::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if cancel.is_cancelled() {
            bail!("Operation cancelled");
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        let end = bytes.saturating_add(count as u64);
        if let Some((offset, limit)) = range {
            let start = bytes.max(offset);
            let stop = end.min(offset.saturating_add(limit));
            if start < stop {
                captured
                    .extend_from_slice(&buffer[(start - bytes) as usize..(stop - bytes) as usize]);
            }
        }
        bytes = end;
    }
    Ok((format!("{:x}", digest.finalize()), bytes, captured))
}
fn read_small(path: &Path) -> Result<Vec<u8>> {
    read_bounded_file(path, FILE_LIMIT).context("Use file_read_chunk for larger files")
}
fn read_bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = open_regular_file(path)?;
    if file.metadata()?.len() > limit {
        bail!("File exceeds the {} byte limit", limit);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("File grew beyond the {} byte limit", limit);
    }
    Ok(bytes)
}
/// Search larger UTF-8 files with bounded per-line memory. An oversized file,
/// line or invalid byte is reported as incomplete rather than as no match.
fn search_large_text(
    path: &Path,
    relative: &Path,
    query: &str,
    limit: usize,
    cancel: &CancellationToken,
) -> Result<(Vec<Value>, bool, Option<&'static str>)> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > SEARCH_FILE_LIMIT {
        return Ok((vec![], false, Some("too_large")));
    }
    let mut reader = std::io::BufReader::new(file);
    let mut line_number = 0usize;
    let mut bytes_read = 0u64;
    let mut found = Vec::new();
    loop {
        if cancel.is_cancelled() {
            bail!("Operation cancelled");
        }
        let mut bytes = Vec::new();
        let count = reader
            .by_ref()
            .take(SEARCH_LINE_LIMIT + 1)
            .read_until(b'\n', &mut bytes)?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(count as u64);
        if bytes_read > SEARCH_FILE_LIMIT {
            return Ok((found, false, Some("too_large")));
        }
        line_number += 1;
        if count as u64 > SEARCH_LINE_LIMIT && !bytes.ends_with(b"\n") {
            return Ok((found, false, Some("line_too_long")));
        }
        let Ok(mut text) = std::str::from_utf8(&bytes) else {
            return Ok((found, false, Some("not_utf8")));
        };
        if let Some(without_newline) = text.strip_suffix('\n') {
            text = without_newline
                .strip_suffix('\r')
                .unwrap_or(without_newline);
        }
        if text.contains(query) {
            found.push(json!({"path":relative,"line":line_number,"text":truncate(text,500)}));
            if found.len() >= limit {
                return Ok((found, true, None));
            }
        }
    }
    Ok((found, false, None))
}
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.into();
    }
    let mut n = max;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    format!("{}\n[output truncated]", &s[..n])
}
struct OutputBudget {
    remaining: usize,
    truncated: bool,
}
struct OutputEvents {
    sender: tokio::sync::mpsc::UnboundedSender<Value>,
    budget: std::sync::Mutex<OutputBudget>,
}
impl OutputEvents {
    fn new(sender: tokio::sync::mpsc::UnboundedSender<Value>) -> Self {
        // Leave room for one truncation notice inside the same 32 KiB budget.
        Self {
            sender,
            budget: std::sync::Mutex::new(OutputBudget {
                remaining: PREVIEW - 256,
                truncated: false,
            }),
        }
    }
    fn emit(&self, stream: &str, mut text: &str) {
        if text.is_empty() {
            return;
        }
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if budget.truncated {
            return;
        }
        while !text.is_empty() {
            let mut count = text.len().min(4096).min(budget.remaining);
            while count > 0 && !text.is_char_boundary(count) {
                count -= 1;
            }
            if count == 0 {
                let _=self.sender.send(json!({"type":"tool_output","stream":stream,"text":"\n[Live output preview truncated; check artifacts and output completeness.]\n","truncated":true}));
                budget.truncated = true;
                return;
            }
            let _ = self
                .sender
                .send(json!({"type":"tool_output","stream":stream,"text":&text[..count]}));
            budget.remaining -= count;
            text = &text[count..];
        }
    }
}

#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}
impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        let mut consumed = 0;
        while consumed < self.pending.len() {
            let remaining = &self.pending[consumed..];
            match std::str::from_utf8(remaining) {
                Ok(valid) => {
                    text.push_str(valid);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&remaining[..valid])
                            .expect("UTF-8 validator identified a valid prefix"),
                    );
                    consumed += valid;
                    if let Some(invalid) = error.error_len() {
                        text.push('\u{fffd}');
                        consumed += invalid;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        text
    }
    fn finish(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned()
    }
}

struct CaptureCompletion {
    text: String,
    error: Option<String>,
    received: u64,
    saved: usize,
    truncated: bool,
}

struct CapturedOutput {
    text: String,
    received: u64,
    saved: usize,
    truncated: bool,
}

async fn settle_capture(
    mut task: tokio::task::JoinHandle<Result<CapturedOutput>>,
    stream: &str,
    deadline: tokio::time::Instant,
) -> CaptureCompletion {
    let error = match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(Ok(Ok(output))) => {
            return CaptureCompletion {
                text: output.text,
                error: None,
                received: output.received,
                saved: output.saved,
                truncated: output.truncated,
            }
        }
        Ok(Ok(Err(error))) => {
            format!("{stream} capture failed: {error:#}; artifact may be incomplete")
        }
        Ok(Err(error)) => {
            format!("{stream} capture task failed: {error}; artifact may be incomplete")
        }
        Err(_) => {
            // Dropping a JoinHandle detaches the task. Explicitly abort and reap
            // it so inherited pipes/file handles cannot leak after the deadline.
            task.abort();
            let _ = task.await;
            format!(
                "{stream} drain timed out; capture task cancelled and artifact may be incomplete"
            )
        }
    };
    CaptureCompletion {
        text: String::new(),
        error: Some(error),
        received: 0,
        saved: 0,
        truncated: false,
    }
}

async fn capture<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    path: PathBuf,
    stream: &'static str,
    events: Option<Arc<OutputEvents>>,
) -> Result<CapturedOutput> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut preview = Vec::new();
    let mut total = 0usize;
    let mut received = 0u64;
    let mut buf = [0u8; 8192];
    let mut decoder = Utf8Decoder::default();
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        received = received.saturating_add(n as u64);
        if let Some(events) = &events {
            events.emit(stream, &decoder.push(&buf[..n]));
        }
        let keep = (ARTIFACT_LIMIT - total).min(n);
        if keep > 0 {
            file.write_all(&buf[..keep]).await?;
            total += keep;
        }
        if preview.len() < PREVIEW {
            let keep = (PREVIEW - preview.len()).min(n);
            preview.extend_from_slice(&buf[..keep]);
        }
    }
    if let Some(events) = &events {
        events.emit(stream, &decoder.finish());
    }
    file.flush().await?;
    let text = String::from_utf8_lossy(&preview).into_owned();
    let truncated = received > total as u64;
    Ok(CapturedOutput {
        text: if received > PREVIEW as u64 {
            format!("{text}\n[preview truncated; see artifact and artifact truncation status]")
        } else {
            text
        },
        received,
        saved: total,
        truncated,
    })
}
fn configure_group(command: &mut Command) {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        command.creation_flags(0x00000200 | 0x08000000);
    }
}
fn kill_pid_group(pid: u32, stop_leader: bool) {
    #[cfg(unix)]
    {
        // Freeze the command's group before inspecting its descendants. If a
        // child is killed while its shell is still running, the shell can
        // otherwise proceed to the next command during cancellation.
        if stop_leader {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGSTOP);
            }
        }
        // Also terminate descendants which created a separate process group
        // (notably Chromium). Only PID/PPID metadata is inspected.
        if let Ok(output) = std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid="])
            .output()
        {
            let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut p = line.split_whitespace();
                    Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?))
                })
                .collect();
            let mut descendants = vec![pid];
            let mut cursor = 0;
            while cursor < descendants.len() {
                let parent = descendants[cursor];
                for (child, parent_id) in &pairs {
                    if *parent_id == parent && !descendants.contains(child) {
                        descendants.push(*child);
                    }
                }
                cursor += 1;
            }
            unsafe {
                if stop_leader {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            for descendant in descendants.into_iter().skip(1).rev() {
                unsafe {
                    libc::kill(descendant as i32, libc::SIGKILL);
                }
            }
        } else {
            unsafe {
                if stop_leader {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = stop_leader;
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
async fn kill_group(child: &mut Child) {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        kill_pid_group(pid, true);
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
    let _ = child.start_kill();
}

pub fn definitions() -> Vec<Value> {
    fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
        json!({"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
    }
    let path = json!({"type":"string","description":"Path on the selected execution device, relative to the workspace or an absolute path inside it."});
    let tab = json!({"type":"string","description":"Browser tab ID from the latest observation; omit for the current tab."});
    let snapshot = json!({"type":"string","description":"snapshot_id from the latest observation or screenshot."});
    let reference =
        json!({"type":"string","description":"Element ref from the latest observation."});
    let coordinate =
        json!({"type":"integer","description":"Coordinate or scroll distance in pixels."});
    vec![
        tool("file_list","List workspace paths with bounded pagination; skips build and dependency directories.",json!({"path":path,"depth":{"type":"integer","minimum":1,"maximum":8,"description":"Maximum directory depth; default 3."},"offset":{"type":"integer","minimum":0,"description":"Zero-based entry offset."},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum entries; default 200."}}),&[]),
        tool("file_read","Read a bounded line range and return a read_token and whole-file SHA-256 for edits. For exact contents use raw:true. For files larger than 2 MiB or binary files, use tool_search to load file_read_chunk.",json!({"path":path,"start_line":{"type":"integer","minimum":1,"description":"First line, 1-based; default 1."},"line_count":{"type":"integer","minimum":1,"maximum":1000,"description":"Number of lines; default 200."},"raw":{"type":"boolean","description":"Return exact entire UTF-8 text, up to 2 MiB."}}),&["path"]),
        tool("file_read_chunk","Read a UTF-8 or base64 byte range from any size file. Continue with next_offset and the returned read_token; offsets count bytes. A changed file requires starting again.",json!({"path":path,"offset":{"type":"integer","minimum":0,"description":"Starting byte offset; default 0."},"limit":{"type":"integer","minimum":4,"maximum":32000,"description":"Maximum bytes; default 6000."},"encoding":{"type":"string","enum":["utf8","base64"],"description":"Use base64 for binary content."},"read_token":{"type":"string","description":"Token from an earlier chunk of the same file."}}),&["path"]),
        tool("file_stat","Inspect a regular file and obtain a version token, including binary or large files, without reading contents into the model. Use before deleting a file.",json!({"path":path}),&["path"]),
        tool("file_search","Search literal text in workspace files with bounded results. Check complete and files_skipped before claiming a match is absent; very large, long-line and unreadable files may be skipped.",json!({"query":{"type":"string","minLength":1,"description":"Literal text to search, not a regular expression."},"path":path,"glob":{"type":"string","description":"Optional workspace-relative file glob."},"limit":{"type":"integer","minimum":1,"maximum":500,"description":"Maximum matches; default 100."}}),&["query"]),
        tool("file_write","Write a file up to 16 MiB; encoding is utf8 by default or base64 for exact binary bytes. Omitting version fields creates only a new file; existing files need a read_token or expected_sha256. Do not supply both.",json!({"path":path,"content":{"type":"string","description":"Complete UTF-8 text or base64-encoded bytes."},"encoding":{"type":"string","enum":["utf8","base64"],"description":"Content encoding; default utf8."},"read_token":{"type":"string","description":"Version token from an inspection of this file."},"expected_sha256":{"type":["string","null"],"description":"Alternative whole-file sha256; null or omitted only when creating a new file."}}),&["path","content"]),
        tool("file_apply_patch","Replace one exact old_text occurrence. Read the file first; use its read_token or expected_sha256, not both. Harness can use a read token from this turn when omitted.",json!({"path":path,"old_text":{"type":"string","minLength":1,"description":"Exact existing text that occurs once."},"new_text":{"type":"string","description":"Exact replacement text."},"read_token":{"type":"string","description":"Version token from the latest file_read for this file."},"expected_sha256":{"type":"string","description":"Alternative whole-file sha256 from file_read."}}),&["path","old_text","new_text"]),
        tool("file_delete","Delete one regular file after file_read; for large or binary files load file_stat via tool_search. Never deletes a directory. A backup is saved. Omit version fields after same-turn inspection so Harness binds the verified token; otherwise supply the actual token or SHA-256, never guess.",json!({"path":path,"read_token":{"type":"string","description":"Version token from file_read or file_stat for this file."},"expected_sha256":{"type":"string","description":"Alternative whole-file SHA-256 from file_read or file_stat."}}),&["path"]),
        tool("file_restore","Restore a file_delete backup to a missing workspace path without overwriting; use the backup_artifact and sha256 returned by file_delete.",json!({"path":path,"backup_artifact":{"type":"string","description":"Backup artifact returned by file_delete."},"expected_sha256":{"type":"string","description":"SHA-256 returned by file_delete."}}),&["path","backup_artifact","expected_sha256"]),
        tool("git_status","Read working tree status.",json!({}),&[]),
        tool("git_diff","Read bounded unstaged workspace diff; check the output artifact and output_complete status.",json!({}),&[]),
        tool("terminal_run","Run a shell command from the workspace. This is a real OS shell, not a filesystem sandbox. Output is bounded, cancellable, and saved as artifacts.",json!({"command":{"type":"string","description":"Shell command to execute in the workspace."},"timeout_ms":{"type":"integer","minimum":100,"maximum":600000,"description":"Timeout in milliseconds; default 120000."}}),&["command"]),
        tool("browser_observe","Observe a managed Chromium page; optionally navigate to url. Returns snapshot_id and element references.",json!({"url":{"type":"string","description":"Optional HTTP(S) address to navigate to."},"tab_id":tab,"depth":{"type":"integer","minimum":1,"description":"Optional accessibility tree depth."}}),&[]),
        tool("browser_screenshot","Capture a real browser screenshot and return image plus artifact.",json!({"tab_id":tab}),&[]),
        tool("browser_act","Act on a browser observation. Except new_tab, provide snapshot_id. click needs ref or screenshot x and y; fill needs ref and text; press needs key.",json!({"action":{"type":"string","enum":["click","fill","press","scroll","back","forward","new_tab","close_tab"],"description":"Action to perform."},"snapshot_id":snapshot,"ref":reference,"text":{"type":"string","description":"Text to fill; empty string clears the field."},"key":{"type":"string","description":"Key to press."},"x":coordinate,"y":coordinate,"url":{"type":"string","description":"Optional HTTP(S) URL for new_tab."},"tab_id":tab}),&["action"]),
        tool("desktop_observe","Observe native desktop windows and accessibility elements on the selected execution device. Requires OS permissions.",json!({"app":{"type":"string","description":"Optional application name."},"window_id":{"type":"string","description":"Optional observed window ID."},"depth":{"type":"integer","minimum":1,"description":"Optional accessibility tree depth."}}),&[]),
        tool("desktop_screenshot","Capture a real native desktop screenshot. Requires OS capture permission.",json!({"window_id":{"type":"string","description":"Optional observed window ID."}}),&[]),
        tool("desktop_act","Act on native desktop. launch needs app; other actions need snapshot_id. click needs ref or screenshot x and y; fill needs ref and text; key needs key.",json!({"action":{"type":"string","enum":["click","fill","key","scroll","launch"],"description":"Action to perform."},"snapshot_id":snapshot,"ref":reference,"text":{"type":"string","description":"Text to fill; empty string clears the field."},"key":{"type":"string","description":"Key or key combination."},"x":coordinate,"y":coordinate,"delta":coordinate,"app":{"type":"string","description":"Application name for launch."}}),&["action"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chunked_reads_preserve_unicode_binary_and_version_boundaries() {
        use base64::Engine as _;
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let text = format!("ab你好🙂{}终", "x".repeat(FILE_LIMIT as usize));
        std::fs::write(workspace.path().join("large.txt"), &text).unwrap();
        let cancel = CancellationToken::new();
        let first = executor
            .execute(
                "file_read_chunk",
                json!({"path":"large.txt","offset":0,"limit":7}),
                cancel.clone(),
            )
            .await
            .unwrap();
        assert_eq!(first["content"], "ab你");
        assert_eq!(first["next_offset"], 5);
        assert_eq!(first["has_more"], true);
        let next = executor.execute("file_read_chunk", json!({"path":"large.txt","offset":first["next_offset"],"limit":7,"read_token":first["read_token"]}), cancel.clone()).await.unwrap();
        assert_eq!(next["content"], "好🙂");
        assert_eq!(next["next_offset"], 12);
        assert!(executor
            .execute(
                "file_read_chunk",
                json!({"path":"large.txt","offset":3,"limit":7,"read_token":first["read_token"]}),
                cancel.clone()
            )
            .await
            .is_err());
        let binary = [0, 255, 5, 10, 128, 3, 77];
        std::fs::write(workspace.path().join("binary.bin"), binary).unwrap();
        let part = executor
            .execute(
                "file_read_chunk",
                json!({"path":"binary.bin","encoding":"base64","limit":4}),
                cancel.clone(),
            )
            .await
            .unwrap();
        let tail = executor.execute("file_read_chunk", json!({"path":"binary.bin","encoding":"base64","offset":part["next_offset"],"limit":4,"read_token":part["read_token"]}), cancel.clone()).await.unwrap();
        let mut assembled = base64::engine::general_purpose::STANDARD
            .decode(part["content"].as_str().unwrap())
            .unwrap();
        assembled.extend(
            base64::engine::general_purpose::STANDARD
                .decode(tail["content"].as_str().unwrap())
                .unwrap(),
        );
        assert_eq!(assembled, binary);
        assert_eq!(tail["has_more"], false);
        let binary_path = workspace.path().join("binary.bin");
        let original_stamp = FileStamp::from_path(&binary_path).unwrap();
        let mut changed = binary;
        changed[6] = 0;
        std::fs::write(&binary_path, changed).unwrap();
        // Force the same metadata even on filesystems with high-resolution
        // clocks. The requested first chunk is unchanged, but the file is not.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&binary_path)
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(original_stamp.modified.unwrap()))
            .unwrap();
        drop(file);
        assert_eq!(FileStamp::from_path(&binary_path).unwrap(), original_stamp);
        assert!(executor
            .execute(
                "file_read_chunk",
                json!({"path":"binary.bin","encoding":"base64","offset":0,"limit":4,"read_token":part["read_token"]}),
                cancel
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn base64_file_writes_preserve_bytes_and_existing_files_need_versions() {
        use base64::Engine as _;
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let cancel = CancellationToken::new();
        let bytes = [0, 255, 1, 2, 128];
        let content = base64::engine::general_purpose::STANDARD.encode(bytes);
        executor
            .execute(
                "file_write",
                json!({"path":"new.bin","encoding":"base64","content":content}),
                cancel.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(workspace.path().join("new.bin")).unwrap(),
            bytes
        );
        assert!(executor
            .execute(
                "file_write",
                json!({"path":"new.bin","encoding":"base64","content":"AAAA"}),
                cancel.clone()
            )
            .await
            .is_err());
        assert!(executor
            .execute(
                "file_write",
                json!({"path":"bad.bin","encoding":"base64","content":"!!"}),
                cancel.clone()
            )
            .await
            .is_err());
        let inspected = executor
            .execute("file_stat", json!({"path":"new.bin"}), cancel.clone())
            .await
            .unwrap();
        executor.execute("file_write", json!({"path":"new.bin","encoding":"base64","content":"AAAA","read_token":inspected["read_token"]}), cancel).await.unwrap();
        assert_eq!(
            std::fs::read(workspace.path().join("new.bin")).unwrap(),
            [0, 0, 0]
        );
    }

    #[tokio::test]
    async fn large_text_can_be_read_in_chunks_and_edited_without_losing_other_bytes() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let original = format!(
            "{}\nsetting=before\n",
            "x".repeat(FILE_LIMIT as usize + 100)
        );
        let path = workspace.path().join("large.txt");
        std::fs::write(&path, &original).unwrap();
        let cancel = CancellationToken::new();
        let offset = original.len() - "setting=before\n".len();
        let page = executor
            .execute(
                "file_read_chunk",
                json!({"path":"large.txt","offset":offset,"limit":100}),
                cancel.clone(),
            )
            .await
            .unwrap();
        assert_eq!(page["content"], "setting=before\n");
        executor.execute("file_apply_patch", json!({"path":"large.txt","old_text":"setting=before","new_text":"setting=after","read_token":page["read_token"]}), cancel).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            original.replacen("setting=before", "setting=after", 1)
        );
    }

    #[tokio::test]
    async fn file_deletion_backs_up_and_restores_large_binary_without_overwriting() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let path = workspace.path().join("binary.dat");
        let mut original = vec![0xff; FILE_LIMIT as usize + 257];
        original[37] = 0;
        std::fs::write(&path, &original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
        }
        let cancel = CancellationToken::new();
        let inspection = executor
            .execute("file_stat", json!({"path":"binary.dat"}), cancel.clone())
            .await
            .unwrap();
        assert_eq!(inspection["bytes"], original.len());
        assert!(executor
            .execute("file_read", json!({"path":"binary.dat"}), cancel.clone())
            .await
            .is_err());
        let deletion = executor
            .execute(
                "file_delete",
                json!({"path":"binary.dat","read_token":inspection["read_token"]}),
                cancel.clone(),
            )
            .await
            .unwrap();
        assert_eq!(deletion["deleted"], true);
        assert!(!path.exists());
        let backup = deletion["backup_artifact"].as_str().unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), original);
        let restore_args = json!({"path":"binary.dat","backup_artifact":backup,"expected_sha256":deletion["sha256"]});
        let mut incorrect = restore_args.clone();
        incorrect["expected_sha256"] = json!("0".repeat(64));
        assert!(executor
            .execute("file_restore", incorrect, cancel.clone())
            .await
            .is_err());
        assert!(!path.exists());
        std::fs::write(backup, b"tampered backup").unwrap();
        assert!(executor
            .execute("file_restore", restore_args.clone(), cancel.clone())
            .await
            .is_err());
        assert!(!path.exists());
        std::fs::write(backup, &original).unwrap();
        let restored = executor
            .execute("file_restore", restore_args.clone(), cancel.clone())
            .await
            .unwrap();
        assert_eq!(restored["restored"], true);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o750
            );
        }
        std::fs::write(&path, b"newer file").unwrap();
        assert!(executor
            .execute("file_restore", restore_args, cancel)
            .await
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"newer file");
    }

    #[tokio::test]
    async fn file_deletion_rejects_stale_versions_directories_and_untrusted_backups() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let path = workspace.path().join("note.txt");
        std::fs::write(&path, "before").unwrap();
        std::fs::write(outside.path().join("outside.txt"), "outside").unwrap();
        let cancel = CancellationToken::new();
        let inspected = executor
            .execute("file_stat", json!({"path":"note.txt"}), cancel.clone())
            .await
            .unwrap();
        std::fs::write(&path, "changed").unwrap();
        assert!(executor
            .execute(
                "file_delete",
                json!({"path":"note.txt","read_token":inspected["read_token"]}),
                cancel.clone()
            )
            .await
            .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed");
        assert_eq!(std::fs::read_dir(artifacts.path()).unwrap().count(), 0);
        #[cfg(unix)]
        {
            let version = executor
                .execute("file_stat", json!({"path":"note.txt"}), cancel.clone())
                .await
                .unwrap();
            std::fs::rename(&path, workspace.path().join("original-inode.txt")).unwrap();
            std::fs::write(&path, "changed").unwrap();
            assert!(executor
                .execute(
                    "file_delete",
                    json!({"path":"note.txt","read_token":version["read_token"]}),
                    cancel.clone()
                )
                .await
                .is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed");
        }
        assert!(executor
            .execute("file_delete", json!({"path":"note.txt"}), cancel.clone())
            .await
            .is_err());
        assert!(executor
            .execute("file_stat", json!({"path":"."}), cancel.clone())
            .await
            .is_err());
        assert!(executor
            .execute(
                "file_stat",
                json!({"path":outside.path().join("outside.txt")}),
                cancel.clone()
            )
            .await
            .is_err());
        assert!(executor.execute("file_restore", json!({"path":"other.txt","backup_artifact":outside.path().join("outside.txt"),"expected_sha256":"0".repeat(64)}), cancel.clone()).await.is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&path, workspace.path().join("link.txt")).unwrap();
            assert!(executor
                .execute("file_stat", json!({"path":"link.txt"}), cancel.clone())
                .await
                .is_err());
            assert!(executor
                .execute(
                    "file_delete",
                    json!({"path":"link.txt","expected_sha256":hash(b"changed")}),
                    cancel.clone()
                )
                .await
                .is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed");
        }
        cancel.cancel();
        assert!(executor
            .execute(
                "file_delete",
                json!({"path":"note.txt","expected_sha256":hash(b"changed")}),
                cancel
            )
            .await
            .is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "changed");
    }

    #[tokio::test]
    async fn search_reports_skipped_files_and_match_limits_as_incomplete() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let mut large = vec![b'x'; FILE_LIMIT as usize + 1];
        large.extend_from_slice(b"\nneedle\n");
        std::fs::write(workspace.path().join("large.txt"), large).unwrap();
        std::fs::write(workspace.path().join("invalid.txt"), [0xff, 0xfe]).unwrap();
        let result = executor
            .execute(
                "file_search",
                json!({"query":"needle"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result["matches"], json!([]));
        assert_eq!(result["complete"], false);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["files_scanned"], 2);
        assert_eq!(result["files_searched"], 0);
        assert_eq!(result["files_skipped"], 2);
        let details = result["skipped_details"].as_array().unwrap();
        assert!(details.iter().any(|item| item["reason"] == "line_too_long"));
        assert!(details.iter().any(|item| item["reason"] == "not_utf8"));

        let streamed_text = format!("{}needle 中文\n", "line\n".repeat(420_000));
        std::fs::write(workspace.path().join("streamed.txt"), streamed_text).unwrap();
        let streamed = executor
            .execute(
                "file_search",
                json!({"query":"needle 中文","glob":"streamed.txt"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(streamed["complete"], true);
        assert_eq!(streamed["files_searched"], 1);
        assert_eq!(streamed["matches"][0]["line"], 420001);
        std::fs::File::create(workspace.path().join("oversized.txt"))
            .unwrap()
            .set_len(SEARCH_FILE_LIMIT + 1)
            .unwrap();
        let oversized = executor
            .execute(
                "file_search",
                json!({"query":"needle","glob":"oversized.txt"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(oversized["complete"], false);
        assert_eq!(oversized["skipped_details"][0]["reason"], "too_large");

        std::fs::write(workspace.path().join("small.txt"), "needle\nneedle\n").unwrap();
        let limited = executor
            .execute(
                "file_search",
                json!({"query":"needle","glob":"small.txt","limit":1}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(limited["match_limit_reached"], true);
        assert_eq!(limited["complete"], false);
        let complete = executor
            .execute(
                "file_search",
                json!({"query":"absent","glob":"small.txt"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(complete["complete"], true);
        assert_eq!(complete["files_searched"], 1);
    }

    #[tokio::test]
    async fn worker_frames_are_bounded_before_newline_and_partial_eof_is_rejected() {
        let bytes = vec![b'x'; 16 * 1024];
        let mut reader = BufReader::new(bytes.as_slice());
        let error = read_worker_line(&mut reader, 1024).await.unwrap_err();
        assert!(error.to_string().contains("1024 byte limit"));
        let mut partial = BufReader::new(b"{\"id\":\"fixture\",\"result\":{}}".as_slice());
        assert!(read_worker_line(&mut partial, 1024)
            .await
            .unwrap_err()
            .to_string()
            .contains("newline-terminated"));
        let mut framed = BufReader::new(b"{\"result\":{}}\nnext\n".as_slice());
        assert_eq!(
            read_worker_line(&mut framed, 1024).await.unwrap(),
            b"{\"result\":{}}\n"
        );
        assert_eq!(
            read_worker_line(&mut framed, 1024).await.unwrap(),
            b"next\n"
        );
    }

    #[cfg(unix)]
    async fn fixture_worker(script: &str) -> Worker {
        fixture_worker_marking(script, None).await
    }

    #[cfg(unix)]
    async fn fixture_worker_marking(script: &str, marker: Option<&Path>) -> Worker {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(marker) = marker {
            command.env("HARNESS_TEST_MARKER", marker);
        }
        configure_group(&mut command);
        let mut child = command.spawn().unwrap();
        Worker {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worker_cancel_and_deadline_cover_a_blocked_stdin_write() {
        for cancelled in [true, false] {
            let mut worker = fixture_worker("sleep 20").await;
            let token = CancellationToken::new();
            let trigger = token.clone();
            if cancelled {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    trigger.cancel();
                });
            }
            let deadline = if cancelled {
                Duration::from_secs(90)
            } else {
                Duration::from_millis(50)
            };
            let started = std::time::Instant::now();
            let outcome = tokio::time::timeout(
                Duration::from_secs(1),
                worker.exchange(
                    "observe",
                    json!({"padding":"x".repeat(1024*1024)}),
                    &token,
                    deadline,
                ),
            )
            .await;
            kill_group(&mut worker.child).await;
            let _ = worker.child.wait().await;
            let error = outcome
                .expect("blocked request write ignored cancellation/deadline")
                .unwrap_err();
            assert!(error.is::<WorkerInterruption>());
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bad_worker_protocol_resets_cache_but_valid_tool_rejection_keeps_it() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        for response in ["not-json", r#"{"id":"wrong-id","result":{}}"#] {
            let script = format!(
                "while IFS= read -r request; do printf '%s\\n' '{}'; done",
                response
            );
            *executor.browser.lock().await = Some(fixture_worker(&script).await);
            assert!(executor
                .execute("browser_observe", json!({}), CancellationToken::new())
                .await
                .is_err());
            assert!(
                executor.browser.lock().await.is_none(),
                "a desynchronized worker must not serve another request"
            );
        }
        let script = r#"while IFS= read -r request; do id=$(printf '%s' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p'); printf '{"id":"%s","error":{"message":"Stale snapshot fixture"}}\n' "$id"; done"#;
        *executor.browser.lock().await = Some(fixture_worker(script).await);
        let error = executor
            .execute("browser_observe", json!({}), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Stale snapshot fixture"));
        let rejected_action = executor
            .execute(
                "browser_act",
                json!({"action":"click"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            rejected_action
                .downcast_ref::<IndeterminateToolOperation>()
                .is_none(),
            "a trustworthy error response is not a lost operation outcome"
        );
        let mut worker = executor
            .browser
            .lock()
            .await
            .take()
            .expect("a valid tool error must preserve browser state");
        kill_group(&mut worker.child).await;
        let _ = worker.child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn helper_side_effect_then_disconnect_is_indeterminate_but_reads_are_not() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let script = r#"IFS= read -r request; printf handled > "$HARNESS_TEST_MARKER"; exit 0"#;
        for (index, (tool, native, args, uncertain)) in [
            ("browser_act", false, json!({"action":"click"}), true),
            (
                "desktop_act",
                true,
                json!({"action":"launch","app":"fixture"}),
                true,
            ),
            ("browser_observe", false, json!({}), false),
            (
                "browser_observe",
                false,
                json!({"url":"http://fixture.invalid"}),
                true,
            ),
            ("browser_screenshot", false, json!({}), false),
            ("desktop_observe", true, json!({}), false),
            ("desktop_screenshot", true, json!({}), false),
            ("desktop_capabilities", true, json!({}), false),
        ]
        .into_iter()
        .enumerate()
        {
            let marker = workspace.path().join(format!("handled-{index}.txt"));
            let worker = fixture_worker_marking(script, Some(&marker)).await;
            let slot = if native {
                &executor.desktop
            } else {
                &executor.browser
            };
            *slot.lock().await = Some(worker);
            let result = if tool == "desktop_capabilities" {
                executor
                    .worker_call(true, "capabilities", args, CancellationToken::new())
                    .await
            } else {
                executor.execute(tool, args, CancellationToken::new()).await
            };
            let error = result.unwrap_err();
            assert_eq!(
                std::fs::read_to_string(&marker).unwrap(),
                "handled",
                "helper must have actually accepted the request before its reply pipe closed"
            );
            assert_eq!(
                error.downcast_ref::<IndeterminateToolOperation>().is_some(),
                uncertain,
                "wrong uncertainty classification for {tool}: {error:#}"
            );
            if let Some(unknown) = error.downcast_ref::<IndeterminateToolOperation>() {
                assert_eq!(unknown.tool, tool);
                assert!(unknown.reason.contains("newline-terminated"));
            }
            assert!(
                slot.lock().await.is_none(),
                "uncertainty must not bypass worker cleanup"
            );
        }
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let error = executor
            .execute("browser_act", json!({"action":"click"}), cancelled)
            .await
            .unwrap_err();
        assert!(
            error.downcast_ref::<IndeterminateToolOperation>().is_none(),
            "cancellation before dispatch has no uncertain side effect"
        );
    }

    #[tokio::test]
    async fn output_drain_deadline_aborts_and_reaps_capture_instead_of_detaching() {
        let artifacts = tempfile::tempdir().unwrap();
        let (reader, mut writer) = tokio::io::duplex(64);
        let task = tokio::spawn(capture(
            reader,
            artifacts.path().join("stuck.txt"),
            "stdout",
            None,
        ));
        let completion = settle_capture(
            task,
            "stdout",
            tokio::time::Instant::now() + Duration::from_millis(50),
        )
        .await;
        assert!(completion.error.unwrap().contains("drain timed out"));
        assert!(
            writer.write_all(b"still alive?").await.is_err(),
            "capture task retained its pipe after the deadline"
        );
    }

    #[tokio::test]
    async fn failed_output_artifacts_cannot_look_like_complete_success() {
        let workspace = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let artifacts = data.path().join("artifacts");
        let executor = Executor::new(workspace.path().into(), artifacts.clone()).unwrap();
        std::fs::remove_dir(&artifacts).unwrap();
        let result = executor
            .execute(
                "terminal_run",
                json!({"command":if cfg!(windows){"exit 0"}else{"true"}}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["output_complete"], false);
        assert_eq!(result["isError"], true);
        assert!(result["stdout_error"]
            .as_str()
            .unwrap()
            .contains("capture failed"));
        assert!(result["stderr_error"]
            .as_str()
            .unwrap()
            .contains("capture failed"));
    }

    #[test]
    fn long_file_read_keeps_the_complete_range_in_a_real_artifact() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("long.txt"), "A".repeat(40_000)).unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let result = executor.file_read(&json!({"path":"long.txt"})).unwrap();
        assert_eq!(result["text_truncated"], true);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["has_more_lines"], false);
        let artifact = result["full_text_artifact"].as_str().unwrap();
        assert_eq!(
            std::fs::read_to_string(artifact).unwrap(),
            format!("1: {}\n", "A".repeat(40_000))
        );
    }

    #[test]
    fn read_token_binds_path_and_still_checks_the_exact_file_version() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("a.txt"), "before").unwrap();
        std::fs::write(workspace.path().join("b.txt"), "before").unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let read = executor.file_read(&json!({"path":"a.txt"})).unwrap();
        let token = read["read_token"].as_str().unwrap();
        assert!(executor
            .file_write(
                &json!({"path":"b.txt","content":"wrong","read_token":token}),
                false
            )
            .is_err());
        executor
            .file_write(
                &json!({"path":"a.txt","content":"after","read_token":token}),
                false,
            )
            .unwrap();
        assert!(executor
            .file_write(
                &json!({"path":"a.txt","content":"stale","read_token":token}),
                false
            )
            .is_err());
        assert!(executor
            .file_write(&json!({"path":"b.txt","content":"bad"}), false)
            .is_err());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "after"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("b.txt")).unwrap(),
            "before"
        );
    }

    #[test]
    fn missing_file_version_creates_only_and_never_overwrites() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let args = json!({"path":"new.txt","content":"first\n"});
        executor.file_write(&args, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("new.txt")).unwrap(),
            "first\n"
        );
        let error = executor
            .file_write(&json!({"path":"new.txt","content":"second"}), false)
            .unwrap_err();
        assert!(error.to_string().contains("read the file again"));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("new.txt")).unwrap(),
            "first\n"
        );
        assert!(executor
            .file_write(
                &json!({"path":"new.txt","old_text":"first","new_text":"wrong"}),
                true
            )
            .is_err());
    }

    #[tokio::test]
    async fn artifact_cap_reports_bytes_actually_saved() {
        let artifacts = tempfile::tempdir().unwrap();
        let mut output = vec![b'A'; ARTIFACT_LIMIT];
        output.extend_from_slice(b"END");
        let path = artifacts.path().join("stdout.txt");
        let captured = capture(&output[..], path.clone(), "stdout", None)
            .await
            .unwrap();
        assert!(captured.truncated);
        assert_eq!(captured.received, (ARTIFACT_LIMIT + 3) as u64);
        assert_eq!(captured.saved, ARTIFACT_LIMIT);
        assert_eq!(
            std::fs::metadata(path).unwrap().len(),
            ARTIFACT_LIMIT as u64
        );
    }

    #[test]
    fn paths_and_hashes() {
        let temp = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let ex = Executor::new(temp.path().into(), artifacts.path().into()).unwrap();
        assert!(ex.resolve("../outside").is_err());
        let made = ex
            .file_write(
                &json!({"path":"a.txt","content":"hello","expected_sha256":null}),
                false,
            )
            .unwrap();
        assert!(ex
            .file_write(
                &json!({"path":"a.txt","content":"bad","expected_sha256":null}),
                false
            )
            .is_err());
        assert!(ex
            .file_write(
                &json!({"path":"a.txt","content":"bad","expected_sha256":"wrong"}),
                false
            )
            .is_err());
        let patched=ex.file_write(&json!({"path":"a.txt","old_text":"hello","new_text":"world","expected_sha256":made["sha256"]}),true).unwrap();
        assert_eq!(patched["sha256"], hash(b"world"));
        let raw = ex.file_read(&json!({"path":"a.txt","raw":true})).unwrap();
        assert_eq!(raw["content"], "world");
        assert_eq!(raw["raw"], true);
        assert_eq!(raw["has_more"], false);
        let backup = patched["backup_artifact"].as_str().unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), b"hello");
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        symlink(outside.path(), temp.path().join("escape")).unwrap();
        let ex = Executor::new(temp.path().into(), artifacts.path().into()).unwrap();
        assert!(ex.resolve("escape/new.txt").is_err());
    }
    #[tokio::test]
    async fn cancellation_stops_process() {
        let temp = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let ex = Executor::new(temp.path().into(), artifacts.path().into()).unwrap();
        let token = CancellationToken::new();
        let trigger = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let result = ex
            .execute(
                "terminal_run",
                json!({"command":if cfg!(windows){"Start-Sleep 20"}else{"sleep 20"}}),
                token,
            )
            .await
            .unwrap();
        assert_eq!(result["cancelled"], true);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_relative_paths_are_in_the_selected_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor = Executor::new(workspace.path().into(), artifacts.path().into()).unwrap();
        let result = executor
            .execute(
                "terminal_run",
                json!({"command":"$ErrorActionPreference='Stop'; Set-Content -LiteralPath marker.txt -Value workspace -Encoding Ascii"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result["exit_code"], 0,
            "unexpected command result: {result}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("marker.txt"))
                .unwrap()
                .trim(),
            "workspace"
        );
    }

    #[tokio::test]
    async fn streaming_delivers_output_before_process_finishes_and_cancels() {
        let workspace = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let executor =
            Arc::new(Executor::new(workspace.path().into(), artifacts.path().into()).unwrap());
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let running = tokio::spawn(async move {
            executor.execute_streaming("terminal_run",json!({"command":if cfg!(windows){"[Console]::Out.Write('first'); [Console]::Out.Flush(); Start-Sleep -Seconds 20"}else{"printf first; sleep 20"}}),token,tx).await
        });
        let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first["type"], "tool_output");
        assert_eq!(first["stream"], "stdout");
        assert!(first["text"].as_str().unwrap().contains("first"));
        assert!(
            !running.is_finished(),
            "output must arrive while the command is still running"
        );
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result["cancelled"], true);
    }

    #[tokio::test]
    async fn streaming_preserves_split_utf8_and_bounds_events_without_truncating_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("output.txt");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = Arc::new(OutputEvents::new(tx));
        let (reader, mut writer) = tokio::io::duplex(1024);
        let text = "中".repeat(20_000);
        let bytes = text.as_bytes().to_vec();
        let expected = bytes.clone();
        let write = tokio::spawn(async move {
            writer.write_all(&bytes[..2]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            writer.write_all(&bytes[2..]).await.unwrap();
            writer.shutdown().await.unwrap();
        });
        capture(reader, artifact.clone(), "stdout", Some(events.clone()))
            .await
            .unwrap();
        write.await.unwrap();
        // stderr shares the same preview cap, including the one-time notice.
        events.emit("stderr", "additional stderr after preview truncation");
        drop(events);
        let mut event_bytes = 0;
        let mut notices = 0;
        let mut visible = String::new();
        while let Some(event) = rx.recv().await {
            let chunk = event["text"].as_str().unwrap();
            assert!(chunk.len() <= 4096);
            assert!(!chunk.contains('\u{fffd}'));
            event_bytes += chunk.len();
            if event["truncated"] == true {
                notices += 1;
            } else {
                visible.push_str(chunk);
            }
        }
        assert!(event_bytes <= PREVIEW);
        assert_eq!(notices, 1);
        assert!(text.starts_with(&visible));
        assert!(!visible.is_empty());
        assert_eq!(std::fs::read(artifact).unwrap(), expected);
    }

    #[tokio::test]
    #[ignore = "requires HARNESS_TEST_BROWSER_CACHE on Unix or downloads Chromium into a temporary cache"]
    async fn browser_managed_setup_and_bridge() {
        use base64::Engine;
        use std::io::{Read, Write};
        let workspace = tempfile::tempdir().unwrap();
        let appdata = tempfile::tempdir().unwrap();
        let executor =
            Executor::new(workspace.path().into(), appdata.path().join("artifacts")).unwrap();
        #[cfg(unix)]
        let existing_cache = std::env::var_os("HARNESS_TEST_BROWSER_CACHE");
        #[cfg(not(unix))]
        let existing_cache: Option<std::ffi::OsString> = None;
        if let Some(cache) = existing_cache {
            #[cfg(unix)]
            std::os::unix::fs::symlink(cache, appdata.path().join("browser-cache")).unwrap();
            #[cfg(not(unix))]
            let _ = cache;
            assert_eq!(executor.browser_status().await["ready"], true);
        } else {
            let setup = executor
                .setup_browser(CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(setup["ready"], true);
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let body="<!doctype html><html><body><h1>Bridge fixture</h1><button onclick=\"document.querySelector('h1').textContent='Clicked through Rust bridge'\">Ready</button></body></html>";
            let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            stream.write_all(response.as_bytes()).unwrap();
        });
        let observed = executor
            .execute(
                "browser_observe",
                json!({"url":format!("http://127.0.0.1:{port}")}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(observed["accessibility"]
            .as_str()
            .unwrap()
            .contains("Bridge fixture"));
        let button = observed["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|element| element["name"] == "Ready")
            .unwrap();
        let action =
            json!({"action":"click","snapshot_id":observed["snapshot_id"],"ref":button["ref"]});
        let clicked = executor
            .execute("browser_act", action.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert!(clicked["accessibility"]
            .as_str()
            .unwrap()
            .contains("Clicked through Rust bridge"));
        let stale = executor
            .execute("browser_act", action, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("Stale snapshot"));
        assert!(stale.downcast_ref::<IndeterminateToolOperation>().is_none());
        assert!(
            executor.browser.lock().await.is_some(),
            "a valid stale-ref rejection must preserve the real Chromium worker"
        );
        let screenshot = executor
            .execute("browser_screenshot", json!({}), CancellationToken::new())
            .await
            .unwrap();
        let image = screenshot["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "image")
            .unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode(image["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            std::fs::read(screenshot["artifact"].as_str().unwrap()).unwrap(),
            png
        );
        server.join().unwrap();
    }
}
