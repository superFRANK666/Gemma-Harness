# 发布流程

项目使用 GitHub Actions 在原生运行环境构建 macOS Apple Silicon、macOS Intel、Windows x64 和 Linux x64 安装包。仓库只保存源码、锁文件和构建脚本；模型、用户会话、编译产物与测试截图不会提交。

1. 先在目标提交运行验证工作流，并检查 `package.json`、`Cargo.toml` 的 workspace 版本及 `apps/desktop/src-tauri/tauri.conf.json` 版本一致。
2. 为该提交推送与版本对应的标签，例如 `v0.3.0`。标签会触发四个独立打包任务。
3. 四个任务全部成功后，发布任务检查两份 DMG、Windows EXE／MSI、Linux DEB／AppImage 共六个非空安装包，并生成 `SHA256SUMS`。
4. 发布任务先创建预发布草稿，上传并核对全部资产的名称和大小，然后才公开。某个平台失败或资产缺失时不会公开不完整的 Release。失败后可在 GitHub Actions 中重新运行失败任务；已公开的 Release 不会被自动覆盖。

流程默认发布为预发布版。需要正式版时，应在完成相应平台验收和签名安排后，明确调整发布标记。构建出来的 macOS 包采用 ad-hoc 签名，不是 Developer ID 公证；Windows 包没有 Authenticode 签名。自动化构建和下载校验不能替代各平台真实安装、运行及桌面权限验收。
