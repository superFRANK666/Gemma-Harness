## 本次更新

- 设置中新增「版本与控制」，可查看当前版本、检查更新并安装已验证签名的新版本。
- 更新会替换当前安装，安装前检查正在运行的任务、远程执行服务和未保存内容；安装失败时保留原有数据并允许重试。
- 加强 Gemma 系列模型的工具调用、文件读取与编辑；增加文件删除备份与恢复、大文件分段读取和二进制读写。
- 延续对聊天模板和采样参数的诊断与配置；真实模型结果及仍存在的限制见仓库中的 Gemma 适配记录。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。原有 0.3.1 用户可以通过应用内更新；0.3.0 没有更新器，需手动安装一次。

这是开发预发布版。macOS 安装包未经过 Apple Developer ID 签名及公证，Windows 安装包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 的真实设备安装及更新仍需分别验收；自动化构建和校验不代表这些设备已完成验收。
