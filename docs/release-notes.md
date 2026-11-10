## 本次更新

- 精简工作台空状态与输入框提示，并更新项目说明。
- 增加应用内更新：启动后和运行期间定期检查，也可在底部手动检查；发现新版本后可下载并安装。更新包会校验签名和版本，安装前会检查未保存内容与正在运行的任务。
- 从 `v0.3.0` 升级需要手动安装本版本一次；该旧版没有自动更新器。之后的受支持安装包可在应用内接收更新。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件的完整性；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。

这是开发预发布版。macOS 软件未经过 Apple Developer ID 签名及公证，Windows 软件未经过 Authenticode 签名；系统可能显示安全提示。Windows、Linux 及 Intel Mac 安装和原生桌面自动化仍需相应设备的实际验收。
