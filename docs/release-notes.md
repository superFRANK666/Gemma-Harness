## v0.3.7（2026-11-17）

- 修复内置终端命令环境与 Shell 调度：在 macOS／Linux 上优先使用用户 POSIX Shell（如 `$SHELL`），并合并登录 Shell、`PATH` 配置目录（`/etc/paths`、`/etc/paths.d`）及 Homebrew／用户工具链路径（`/opt/homebrew/bin`、`/usr/local/bin`、`~/.cargo/bin`、`~/.local/bin` 等），解决桌面应用从图形界面启动时找不到 `brew` 等命令行工具的问题（同步覆盖 Git 与 MCP 子进程）。
- 修复内置终端输入框在 macOS WebKit 下的自动首字母大写、自动纠错与智能破折号／智能引号替换（如 `brew --config` 被改写为 `Brew —config`），并保留粘贴与输入法组合输入。
- 在内置终端标题栏展示当前绑定的工作目录路径，并为 `streamTool` 增加结果 `stdout`／`stderr` 兜底渲染（当未收到流式分片时补齐输出且避免重复），提升命令反馈一致性。
- 同步更新 macOS、Windows、Linux 安装包与签名更新清单至 0.3.7。

本版在 Apple Silicon macOS 上完成了前端 `tsc -b`／`vite build`、72 项 Playwright UI 自动化测试、发布脚本测试（3 项），以及 `cargo fmt --check`、`cargo check -p harness-desktop`、`cargo clippy`、`harness-executor`、`harness-model`、`harness-remote`、`harness-core` 单元测试与 `manual_stream`／`projects` 集成测试；`agent_end_to_end` 中既有步骤上限断言测试（期待 48 与当前 `MAX_STEPS = 96` 不一致）未修改。三平台生产构建、安装包签名与更新清单校验以 GitHub Actions 发布流水线为准，Windows、Linux 与 Intel Mac 的真实设备安装与更新仍需分别验收。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。原有 0.3.1 及后续版本用户可以通过应用内更新；0.3.0 没有更新器，需手动安装一次。

这是开发预发布版。macOS 安装包未经过 Apple Developer ID 签名及公证，Windows 安装包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 的真实设备安装及更新仍需分别验收；自动化构建和校验不代表这些设备已完成验收。
