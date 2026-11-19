## v0.3.12（2026-11-19）

- 优化本地模型默认配置：新建托管引擎默认使用 32K 上下文并自动分配适合设备的 GPU 层，保留已有配置中用户指定的上下文和 GPU 层数。大型模型冷启动最多等待 15 分钟，并保留取消与退出处理。
- 改善上下文和输出预算：自动输出上限按已加载窗口及本次输入调整，最多分配窗口的三分之一、上限为 16,384 token；保留用户显式设置，并为历史摘要单独分配预算。
- 改善长回复的显示与传输：首个回答和思考片段立即显示，后续长回复合并刷新；持续接收数据的流式回复按空闲时长而非总时长限制。同步减少 GGUF 元数据、请求数据和 SSE 解析中的临时复制。
- 更新 macOS、Windows、Linux 安装包和签名更新清单至 0.3.12。

本次发布按要求未运行测试或生成性能基准，因此实际首字延迟和生成速度仍需测量。三平台生产构建、安装包签名与更新清单校验结果以 GitHub Actions 发布流程为准；Windows、Linux 和 Intel Mac 的真实设备安装与更新，以及目标模型的实际任务效果仍需分别验收。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。原有 0.3.1 及后续版本用户可以通过应用内更新；0.3.0 没有更新器，需手动安装一次。

这是开发预发布版。macOS 安装包未经过 Apple Developer ID 签名及公证，Windows 安装包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 的真实设备安装及更新仍需分别验收；自动化构建和校验不代表这些设备已完成验收。
