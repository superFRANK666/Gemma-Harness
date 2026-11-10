# 发布与应用内更新

项目使用 GitHub Actions 在原生环境构建 macOS Apple Silicon、macOS Intel、Windows x64 和 Linux x64 安装包。仓库只保存源码、锁文件和构建脚本；模型、用户会话、签名私钥与构建产物不得提交。

## 更新机制

应用启动后及每六小时检查一次更新，底部「检查更新」也可以手动触发。用户选择下载并安装后，更新器会验证签名及其绑定的版本。若消息、图片、终端命令或文件尚未保存，或有任务正在运行，安装会等待用户处理；安装时应用重启。更新检查失败不会显示为已是最新版本。

当前发布渠道包括预发布版。更新清单发布在 `update-channel` 分支的 `latest.json`，不使用会忽略预发布版的 GitHub `releases/latest`。清单分别提供 macOS ARM64／Intel、Windows NSIS／MSI 及 Linux AppImage／DEB 的签名包。更新器只接受版本高于当前版本且与签名绑定的版本，不允许回退。`v0.3.0` 没有更新器，必须先手动安装带更新功能的版本。

更新签名公钥保存在 `apps/desktop/src-tauri/tauri.conf.json`；私钥及密码保存在仓库 Actions 的 `TAURI_SIGNING_PRIVATE_KEY` 和 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 密钥中，并应由维护者安全备份。丢失私钥会使已安装应用无法验证后续更新；不要轮换或覆盖它而不制定迁移方案。更新包的签名验证与 macOS Developer ID 公证、Windows Authenticode 签名是不同机制。

## 发布步骤

1. 更新三个版本号（`package.json`、workspace `Cargo.toml`、`tauri.conf.json`）与 `docs/release-notes.md`；版本必须是递增的数字 SemVer。普通本地构建不需要私钥。
2. 将发布提交推送到 `main`，等待该提交的三平台 Verify 工作流通过，再推送对应标签，例如 `v0.3.1`。
3. 标签触发四个平台打包。构建任务使用 Actions 密钥生成更新包，并对 Windows、Linux 和 macOS 更新资产逐个签名，签名绑定版本号。
4. 发布任务检查六个安装包、两份 macOS 更新归档和六份签名；生成 `latest.json` 与 `SHA256SUMS`，并用嵌入应用的公钥逐一验证签名和版本。
5. 工作流先上传预发布草稿，检查全部资产的远端 SHA256 和大小，再公开 Release，最后原子更新 `update-channel/latest.json` 并回读。渠道不允许覆盖较新的版本；相同版本若内容不同也会拒绝。

失败后可重新运行失败任务。已公开的 Release 不会被覆盖；若其资产与本次产物完全一致，重跑发布任务可以继续补发更新清单，否则会停止。macOS 安装包仍是 ad-hoc 签名、未经公证；Windows 安装包未经 Authenticode 签名。自动化构建与签名检查不能替代 Windows、Linux、Intel Mac 上的真实安装和更新验收。
