## v0.3.11（2026-11-19）

- 任务运行时可切换历史会话，也可通过侧栏或 `⌘/Ctrl + N` 新建会话并提交消息。运行中的会话在侧栏显示进度图标。
- 回复、工具输出、输入草稿和停止操作归属各自的会话。再次打开运行中的会话可继续查看进度；使用同一推理服务的请求沿用服务的排队机制。

本次发布未运行测试。三端安装包及签名更新清单由 GitHub Actions 构建和验证。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。原有 0.3.1 及后续版本用户可以通过应用内更新；0.3.0 没有更新器，需手动安装一次。

这是开发预发布版。macOS 安装包未经过 Apple Developer ID 签名及公证，Windows 安装包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 的真实设备安装及更新仍需分别验收；自动化构建和校验不代表这些设备已完成验收。
