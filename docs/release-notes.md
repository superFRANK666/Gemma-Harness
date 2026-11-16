## v0.3.6（2026-11-16）

- 优化工作台前端交互与视觉排版：提升状态栏与辅助文本字号和对比度，统一设置页面卡片分组与按钮对齐，完善消息与代码块一键复制、工具状态语义徽标及图片附件缩略图。
- 升级检查器面板：文件浏览器支持目录返回上级、未保存标记、`Cmd/Ctrl+S` 快捷保存与放弃编辑确认；差异视图将 `git_diff` 结果解析为带行增删高亮的结构化 Diff；电脑视图支持点击放大截图预览。
- 完善工作台快捷键与面板交互：顶栏菜单按钮支持收起／展开侧边栏，终端支持上下键命令历史与清空，会话列表支持标题重命名、JSON 导出以及 `Esc` 清空并退出搜索，设置与性能／更新浮层支持 `Esc` 关闭。
- 修复全局快捷键闭包读取旧配置、流式/历史复用 `callId` 冲突及无增量直出 `assistant_completed` 等边界问题，并同步修复及扩展前端 Playwright UI 自动化测试。
- 优化应用内手动检查更新机制：显式携带无缓存请求头以穿透本地与中间代理缓存，并在已安装待重启状态锁定重复检查入口，避免状态被覆盖。
- 同步更新 macOS、Windows、Linux 安装包与签名更新清单至 0.3.6。

本版已完成前端 TypeScript 类型检查、Vite 生产构建、Playwright UI 自动化测试、发布脚本测试，以及 Rust `cargo fmt`、`cargo check` 与 `cargo clippy` 检查。Windows、Linux 和 Intel Mac 的真实设备安装与更新仍需分别验收。

## 下载

- macOS 14+：Apple Silicon 下载 `macOS-arm64.dmg`；Intel 下载 `macOS-x64.dmg`。
- Windows x64：下载 `Windows-x64-setup.exe` 或 `Windows-x64.msi`。
- Linux x64：Debian／Ubuntu 下载 `.deb`，其他兼容环境可尝试 `.AppImage`；后者可能需要 FUSE 2 运行库（Ubuntu 24.04 为 `libfuse2t64`）。
- `SHA256SUMS` 可用于核对下载文件；`.sig`、`latest.json` 和 macOS `.app.tar.gz` 供应用更新器使用。原有 0.3.1 及后续版本用户可以通过应用内更新；0.3.0 没有更新器，需手动安装一次。

这是开发预发布版。macOS 安装包未经过 Apple Developer ID 签名及公证，Windows 安装包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 的真实设备安装及更新仍需分别验收；自动化构建和校验不代表这些设备已完成验收。
