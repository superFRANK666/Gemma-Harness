## v0.3.10（2026-11-18）

- 在最近会话菜单中增加删除入口：显示会话名称并要求二次确认，默认聚焦“取消”。确认后永久删除该会话的消息与操作记录，无法撤销；项目和共享产物保留。
- 删除当前会话后返回新会话；运行中的任务或终端操作会阻止删除，并在工作区将变化时保护未保存的文件编辑。会话列表更新后会丢弃过期快照，避免已删除或重命名的会话重新出现。
- 更新 macOS、Windows、Linux 安装包和签名更新清单至 0.3.10。

本次发布按要求未运行测试。已有 `agent_end_to_end` 步骤上限断言仍期望 48，而当前代码上限为 96，本次未执行该测试。三平台生产构建、安装包签名与更新清单校验结果以 GitHub Actions 发布流程为准；Windows、Linux 和 Intel Mac 的真实设备安装与更新，以及目标模型的实际效果仍需分别验收。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。原有 0.3.1 及后续版本用户可以通过应用内更新；0.3.0 没有更新器，需手动安装一次。

这是开发预发布版。macOS 安装包未经过 Apple Developer ID 签名及公证，Windows 安装包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 的真实设备安装及更新仍需分别验收；自动化构建和校验不代表这些设备已完成验收。
