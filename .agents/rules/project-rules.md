---
trigger: always_on
---

# Harness 项目规则

## 1. Git 提交、推送与三端自动 Release
- **远端仓库**：`origin` 指向 `https://github.com/superFRANK666/Gemma-Harness`。
- **修改即提交并推送**：每次完成修改并通过必要检查后，先核对 `git status` 与 `git diff`（严禁混入 `.harness/`、临时产物、模型权重或凭据），再以清晰准确的提交说明执行 `git commit` 并推送至 `origin`。
- **同步触发三端 Release（Windows / macOS / Linux）**：每次推送到远端 GitHub 后，必须同步完成/触发包含 **Windows（`windows-x64`）、macOS（`macos-arm64` / `macos-x64`）、Linux（`linux-x64`）** 三端安装包与签名更新清单（`update-channel`）的 GitHub Release 发布（见 `.github/workflows/release.yml`）。
  - 遵循仓库版本校验约束：保持 `package.json`、`Cargo.toml`（`workspace.package.version`）、`apps/desktop/src-tauri/tauri.conf.json` 的 SemVer 版本号一致且单调递增，并同步更新 `Cargo.lock`、`package-lock.json` 与 `docs/release-notes.md`。
  - 推送 `main` 后配合对应 `v<version>` 标签（或已配置的自动触发流水线）完成三端打包、签名验证与 Release 发布；若受限于环境权限无法推送/打标，必须明确告知阻塞原因与待执行命令。

## 2. 架构边界、性能与简洁度标准
- **分层职责单一**：严格维护现有模块边界——React 前端只作展示层（无本地凭据或越权逻辑），`harness-model` 负责模型协议与诊断，`harness-executor` 负责本地工具与进程隔离执行，`harness-remote` 负责证书钉扎远端传输，`harness-core` 负责 Agent 编排、SQLite 持久化与引擎托管。
- **最佳性能与最简洁写法**：算法、数据结构、并发/异步调度与锁粒度追求最优时间与空间开销；代码表达直接明了、浅层嵌套、早返回，避免无意义克隆、冗余序列化与冗余状态同步。
- **杜绝“屎山代码”**：严禁复制粘贴式重复逻辑、打补丁式特判堆砌、混乱耦合、死代码以及无意义的过度封装；同类分支优先通过表驱动或共享辅助函数统一处理，同时保留既有文档注释与安全不变量。

## 3. 编辑后自我审视与验证闭环
- **先反思再收尾**：每次编辑完成后，主动复盘本次改动——审视是否还能用更优的算法/数据结构、更精简的抽象、更低的耦合或更严谨的边界处理进一步提升；若发现改进空间，立即精炼后再收尾。
- **实事求是验证**：提交前运行与改动匹配的校验命令（如 `npm run typecheck` / `npm run build`、`cargo fmt --check`、`cargo clippy`、`cargo test`、`npm run test:release`），只将最后一次编辑后实际运行且全部通过的结果报告为通过，严禁虚构或推测未运行项。
