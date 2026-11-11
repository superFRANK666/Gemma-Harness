## v0.3.4（2026-11-11）

- 将「模型连接」和「本地引擎」合并为「模型与推理」，统一管理模型列表、默认模型、推理参数和诊断。
- 提供「加载本地模型」和「连接已有服务」两条接入路径；本地模型直接选择 GGUF 和可选视觉 projector，已有服务支持 LM Studio 和兼容 API。
- 本地引擎的安装、GPU 层数、端口和自定义模板收进高级设置；服务地址与模型 ID 自动管理，上下文在启动后同步，避免重复配置。
- 修正未启动的本地引擎配置无法保存的问题；运行期间锁定模型及加载参数，停止后即可修改，后端同步校验运行状态。
- 本地推理启动成功后自动选为默认模型，并保留其余模型连接。macOS、Windows、Linux 安装包及应用内升级清单同步更新至 0.3.4。

本版发布流程执行生产构建，以及安装包、更新签名和校验和核对；按要求未编写或运行测试。模型诊断由用户手动触发，本次没有运行模型诊断或真实设备验收。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。原有 0.3.1／0.3.2 用户可以通过应用内更新；0.3.0 没有更新器，需手动安装一次。

这是开发预发布版。macOS 安装包未经过 Apple Developer ID 签名及公证，Windows 安装包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 的真实设备安装及更新仍需分别验收；自动化构建和校验不代表这些设备已完成验收。
