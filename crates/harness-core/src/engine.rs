use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
};

pub const RUNTIME_MANIFEST: &str = include_str!("../../../assets/llama-runtime.json");

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfig {
    pub binary_path: String,
    pub model_path: String,
    #[serde(default)]
    pub projector_path: String,
    /// Explicit opt-in override. Empty means use the GGUF's embedded template.
    #[serde(default)]
    pub chat_template_path: String,
    #[serde(default = "default_context")]
    pub context_length: usize,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_gpu")]
    pub gpu_layers: i32,
}
fn default_context() -> usize {
    8192
}
fn default_port() -> u16 {
    1235
}
fn default_gpu() -> i32 {
    99
}

pub struct Engine {
    child: Mutex<Option<Child>>,
    dir: PathBuf,
}
impl Engine {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            child: Mutex::new(None),
            dir,
        }
    }
    pub fn status(&self) -> Result<Value> {
        let mut child = self.child.lock().unwrap();
        let running = if let Some(c) = child.as_mut() {
            c.try_wait()?.is_none()
        } else {
            false
        };
        if !running {
            *child = None;
        }
        Ok(
            json!({"running":running,"pid":child.as_ref().map(|c|c.id()),"pinnedInstallerVersion":"b12211","actualRuntimeVersion":"unknown_until_probed","logPath":self.dir.join("engine.log")}),
        )
    }
    /// Report only known startup warnings, never arbitrary log content. A
    /// bounded prefix avoids reading an indefinitely growing engine log.
    pub fn startup_warnings(&self) -> Value {
        const LIMIT: u64 = 1024 * 1024;
        let path = self.dir.join("engine.log");
        let Ok(file) = std::fs::File::open(path) else {
            return json!({"status":"unavailable","source":"managed_engine_log"});
        };
        let truncated = file.metadata().ok().is_some_and(|m| m.len() > LIMIT);
        let mut bytes = Vec::new();
        if file.take(LIMIT).read_to_end(&mut bytes).is_err() {
            return json!({"status":"unavailable","source":"managed_engine_log"});
        }
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        let mut warnings = Vec::new();
        if text.contains("outdated gemma4 chat template") {
            warnings.push(json!({"code":"outdated_gemma4_chat_template","message":"llama.cpp reports an outdated Gemma 4 chat template and applies compatibility workarounds. Verify the model's template and tool-calling behavior."}));
        }
        if text.contains("control-looking token") {
            warnings.push(json!({"code":"control_token_type_overridden","message":"llama.cpp reports a control-looking token whose type it overrides. Verify the actual model and tool-calling behavior."}));
        }
        json!({"status":"inspected","source":"managed_engine_log_prefix","maxBytes":LIMIT,"truncated":truncated,"warnings":warnings})
    }
    pub fn start(&self, config: &EngineConfig) -> Result<Value> {
        if self.status()?["running"] == true {
            bail!("推理引擎已经运行")
        }
        if !Path::new(&config.model_path).is_file() {
            bail!("请选择有效的本地 GGUF 文件")
        }
        if !(2048..=262144).contains(&config.context_length) {
            bail!("上下文必须在 2048 到 262144 之间")
        }
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", config.port)).context("端口已被占用")?;
        drop(listener);
        std::fs::create_dir_all(&self.dir)?;
        let template = prepare_chat_template(&self.dir, &config.chat_template_path)?;
        let log = std::fs::File::create(self.dir.join("engine.log"))?;
        let mut cmd = Command::new(&config.binary_path);
        cmd.args([
            "--model",
            &config.model_path,
            "--alias",
            "gemma-local",
            "--host",
            "127.0.0.1",
            "--port",
            &config.port.to_string(),
            "--ctx-size",
            &config.context_length.to_string(),
            "--n-gpu-layers",
            &config.gpu_layers.to_string(),
            "--jinja",
            "--parallel",
            "1",
        ]);
        if !config.projector_path.is_empty() {
            cmd.args(["--mmproj", &config.projector_path]);
        }
        if let Some((path, _)) = &template {
            cmd.arg("--chat-template-file").arg(path);
        }
        if let Some(parent) = Path::new(&config.binary_path).parent() {
            cmd.current_dir(parent);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let child = cmd
            .spawn()
            .context("无法启动 llama-server，请先安装引擎或选择可执行文件")?;
        *self.child.lock().unwrap() = Some(child);
        let mut status = self.status()?;
        status["chatTemplate"] = match template {
            Some((_, report)) => report,
            None => json!({"source":"gguf_embedded","runtimeApplied":"unverified"}),
        };
        Ok(status)
    }
    pub fn stop(&self) -> Result<Value> {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.status()
    }
    pub async fn install(&self, backend: &str) -> Result<Value> {
        let manifest: Value = serde_json::from_str(RUNTIME_MANIFEST)?;
        let suffix = runtime_suffix(backend)?;
        let assets = manifest["assets"].as_array().context("无运行时清单")?;
        let primary = assets
            .iter()
            .find(|a| {
                a["name"].as_str().unwrap_or("").starts_with("llama-")
                    && a["name"].as_str().unwrap_or("").ends_with(&suffix)
            })
            .context("固定运行时没有此平台构建")?;
        let destination = self.dir.join("runtimes").join("b12211").join(backend);
        std::fs::create_dir_all(&destination)?;
        let mut selected = vec![primary];
        if backend == "cuda" {
            if let Some(runtime) = assets.iter().find(|a| {
                a["name"].as_str().unwrap_or("").starts_with("cudart-")
                    && a["name"].as_str().unwrap_or("").ends_with(&suffix)
            }) {
                selected.push(runtime);
            }
        }
        for asset in selected {
            let url = asset["browser_download_url"]
                .as_str()
                .context("缺少运行时下载地址")?;
            let digest = asset["digest"]
                .as_str()
                .and_then(|d| d.strip_prefix("sha256:"))
                .context("固定运行时缺少 SHA256，拒绝下载")?;
            let response = reqwest::Client::new()
                .get(url)
                .send()
                .await?
                .error_for_status()?;
            let bytes = response.bytes().await?;
            if bytes.len() > 1024 * 1024 * 1024 {
                bail!("运行时包过大")
            }
            if hex::encode(Sha256::digest(&bytes)) != digest {
                bail!("运行时 SHA256 校验失败")
            }
            let dest = destination.clone();
            let is_zip = url.ends_with(".zip");
            tokio::task::spawn_blocking(move || -> Result<()> {
                if is_zip {
                    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
                    for i in 0..archive.len() {
                        let mut entry = archive.by_index(i)?;
                        let name = entry.enclosed_name().context("运行时包包含非法路径")?;
                        let path = dest.join(name);
                        if entry.is_dir() {
                            std::fs::create_dir_all(&path)?;
                        } else {
                            if let Some(p) = path.parent() {
                                std::fs::create_dir_all(p)?;
                            }
                            std::io::copy(&mut entry, &mut std::fs::File::create(&path)?)?;
                        }
                    }
                } else {
                    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
                    let mut archive = tar::Archive::new(decoder);
                    for entry in archive.entries()? {
                        let mut entry = entry?;
                        if !entry.unpack_in(&dest)? {
                            bail!("运行时包含非法路径")
                        }
                    }
                }
                Ok(())
            })
            .await??;
        }
        let binary = find_binary(&destination).context("解压后没有找到 llama-server")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
        }
        let version = Command::new(&binary)
            .arg("--version")
            .output()
            .context("运行时安装后无法执行")?;
        if !version.status.success() {
            bail!(
                "运行时自检失败：{}",
                String::from_utf8_lossy(&version.stderr)
            )
        }
        Ok(
            json!({"binaryPath":binary,"version":"b12211","backend":backend,"verifiedSha256":true,"versionOutput":format!("{}{}",String::from_utf8_lossy(&version.stdout),String::from_utf8_lossy(&version.stderr))}),
        )
    }
}

/// Snapshot an explicit template choice, so edits to the user's original file
/// after startup cannot change what is passed to llama.cpp. Do not copy or
/// expose arbitrary logs, and never overwrite an existing snapshot.
fn prepare_chat_template(dir: &Path, source: &str) -> Result<Option<(PathBuf, Value)>> {
    const LIMIT: u64 = 256 * 1024;
    if source.trim().is_empty() {
        return Ok(None);
    }
    let file = std::fs::File::open(source).context("无法读取自定义聊天模板")?;
    if !file.metadata()?.is_file() {
        bail!("自定义聊天模板必须是普通文件")
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > LIMIT || std::str::from_utf8(&bytes).is_err() {
        bail!("自定义聊天模板必须是非空 UTF-8 文件，大小不超过 256 KiB")
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let templates = dir.join("chat-templates");
    std::fs::create_dir_all(&templates)?;
    let snapshot = templates.join(format!("{hash}.jinja"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&snapshot) {
        Ok(mut target) => {
            if let Err(error) = target.write_all(&bytes).and_then(|_| target.sync_all()) {
                let _ = std::fs::remove_file(&snapshot);
                return Err(error).context("无法保存聊天模板快照");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&snapshot)?;
            if !metadata.file_type().is_file()
                || metadata.len() != bytes.len() as u64
                || std::fs::read(&snapshot)? != bytes
            {
                bail!("现有聊天模板快照不匹配，未启动推理引擎")
            }
        }
        Err(error) => return Err(error).context("无法创建聊天模板快照"),
    }
    let known_issues = harness_model::inspect_template_issues(std::str::from_utf8(&bytes)?);
    Ok(Some((
        snapshot,
        json!({"source":"custom_file_snapshot","sha256":hash,"bytes":bytes.len(),"runtimeApplied":"unverified","knownIssues":known_issues}),
    )))
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn find_binary(path: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_binary(&p) {
                return Some(found);
            }
        } else if p.file_name().and_then(|x| x.to_str())
            == Some(if cfg!(windows) {
                "llama-server.exe"
            } else {
                "llama-server"
            })
        {
            return Some(p);
        }
    }
    None
}
fn runtime_suffix(backend: &str) -> Result<String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    if cfg!(target_os = "macos") {
        return Ok(format!("macos-{arch}.tar.gz"));
    }
    if cfg!(windows) {
        return Ok(match backend {
            "cuda" => format!("win-cuda-12.4-{arch}.zip"),
            "vulkan" => format!("win-vulkan-{arch}.zip"),
            "cpu" => format!("win-cpu-{arch}.zip"),
            _ => bail!("Windows 后端应为 cuda/vulkan/cpu"),
        });
    }
    Ok(match backend {
        "cuda" => format!("ubuntu-cuda-12.8-{arch}.tar.gz"),
        "vulkan" => format!("ubuntu-vulkan-{arch}.tar.gz"),
        "cpu" => format!("ubuntu-{arch}.tar.gz"),
        _ => bail!("Linux 后端应为 cuda/vulkan/cpu"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_warnings_are_bounded_and_do_not_expose_the_engine_log() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into());
        assert_eq!(engine.startup_warnings()["status"], "unavailable");
        std::fs::write(dir.path().join("engine.log"), b"PRIVATE_MARKER detected an outdated gemma4 chat template, applying compatibility workarounds\ncontrol-looking token\n").unwrap();
        let result = engine.startup_warnings();
        assert_eq!(result["warnings"].as_array().unwrap().len(), 2);
        assert!(!result.to_string().contains("PRIVATE_MARKER"));
        std::fs::write(dir.path().join("engine.log"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        let bounded = engine.startup_warnings();
        assert_eq!(bounded["truncated"], true);
        assert_eq!(bounded["warnings"], json!([]));
    }

    #[test]
    fn custom_template_is_bounded_snapshotted_and_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        assert!(prepare_chat_template(dir.path(), "").unwrap().is_none());
        let source = dir.path().join("source.jinja");
        std::fs::write(&source, "<|turn>user\n{{ message }}").unwrap();
        let (snapshot, report) = prepare_chat_template(dir.path(), source.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(report["source"], "custom_file_snapshot");
        assert_eq!(report["runtimeApplied"], "unverified");
        assert_eq!(
            report["sha256"],
            format!("{:x}", Sha256::digest(b"<|turn>user\n{{ message }}"))
        );
        std::fs::write(&source, "changed").unwrap();
        assert_eq!(
            std::fs::read_to_string(&snapshot).unwrap(),
            "<|turn>user\n{{ message }}"
        );
        std::fs::write(&source, []).unwrap();
        assert!(prepare_chat_template(dir.path(), source.to_str().unwrap()).is_err());
        std::fs::write(&source, vec![b'x'; 256 * 1024 + 1]).unwrap();
        assert!(prepare_chat_template(dir.path(), source.to_str().unwrap()).is_err());
        std::fs::write(&source, [0xff]).unwrap();
        assert!(prepare_chat_template(dir.path(), source.to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn managed_engine_passes_the_snapshot_as_an_explicit_argument() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fake-server");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args.tmp\" && mv \"$(dirname \"$0\")/args.tmp\" \"$(dirname \"$0\")/args.txt\"\nexec sleep 10\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let model = dir.path().join("model.gguf");
        let source = dir.path().join("source.jinja");
        std::fs::write(&model, b"fixture").unwrap();
        std::fs::write(&source, "fixture template").unwrap();
        let engine = Engine::new(dir.path().join("runtime"));
        let config = EngineConfig {
            binary_path: executable.to_string_lossy().into_owned(),
            model_path: model.to_string_lossy().into_owned(),
            projector_path: String::new(),
            chat_template_path: source.to_string_lossy().into_owned(),
            context_length: 4096,
            port: 0,
            gpu_layers: 0,
        };
        let status = engine.start(&config).unwrap();
        let args_file = dir.path().join("args.txt");
        for _ in 0..100 {
            if args_file.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let args = std::fs::read_to_string(args_file).unwrap();
        let values: Vec<_> = args.lines().collect();
        let index = values
            .iter()
            .position(|arg| *arg == "--chat-template-file")
            .unwrap();
        assert_ne!(values[index + 1], source.to_string_lossy());
        assert_eq!(
            std::fs::read_to_string(values[index + 1]).unwrap(),
            "fixture template"
        );
        assert_eq!(status["chatTemplate"]["source"], "custom_file_snapshot");
        engine.stop().unwrap();
    }
}
