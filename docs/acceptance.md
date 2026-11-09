# 0.3.0 开发版验收记录

日期：2026-11-09，Asia/Shanghai。此文档记录该日期的阶段性验收；仓库中的历史证据路径使用 `$HOME` 代替本机用户主目录，测试读取令牌使用可关联的占位符代替。

**当前结论：已实现并打包 macOS Apple Silicon 开发版，工程测试及原生工作台交互验收通过。指定模型和 Harness 自身的三平台桌面自动化验收尚未全部完成，不能据此宣称“完美适配”或正式三平台发行。**

## 已完成的验证

| 项目 | 实际结果 | 验证边界 |
|---|---|---|
| Rust workspace | 125 项通过，0 失败，1 项浏览器测试默认忽略 | 37 项 core 单元、17 项 Agent、4 项项目管理集成、1 项诊断取消、1 项手动终端、1 项 MCP 初始化取消、11 项 Executor、43 项模型／GGUF、10 项远端协议 |
| Clippy | `--workspace --all-targets -- -D warnings` 通过 | 本机 macOS 工具链，不等于完整 Windows／Linux 应用已构建 |
| 前端 | TypeScript／Vite 构建通过；34 项 Playwright UI 测试通过 | 新增 9 项外观、4 项项目管理；保留性能／预算、取消及绑定隔离回归；使用显式 IPC fixture |
| macOS 原生工作台 | 0.3.0 外观预览／保存／重启、项目注册／改名／移除通过 | 真实 Rust IPC；磁盘核对测试文件保留，重启后已移除项目未恢复；原工作目录及默认外观已恢复 |
| 实际 Chromium | 2 项通过，无跳过 | 真实本地页面、按钮操作、旧引用拒绝、PNG、取消、密码快照脱敏和 URL 限制 |
| 浏览器安装＋Rust bridge | 0.2.0 显式用现有缓存通过真实 Rust→worker→Chromium 集成 | 观察、点击、旧引用拒绝后可继续使用、PNG／artifact 字节核对；首次空缓存下载在 0.1.0 已验证，约 400 秒 |
| llama.cpp 安装 | 官方 b12211 下载、SHA256 校验、可执行自检通过 | 未加载大模型权重；不证明目标量化的任务质量 |
| 推理设备发现 | 实际列出 Apple M4 Metal 与 Accelerate | 不是 Windows 主机的 GPU／显存信息 |
| GGUF 身份检查 | 合成格式夹具测试通过 | 仅读取元数据／模板指纹，不将其伪称为全权重哈希认证 |
| Windows／Linux helper | 原生 helper 既有交叉检查通过；本轮重新检查 0.2.0 Executor 两个目标通过 | 未在对应系统执行截图、输入、Portal 或打包安装 |
| macOS 兼容目标 | 主应用、Swift helper 的 Mach-O 最低系统版本均为 14.0 | 实际开发机为 M4／macOS 27.0.1；尚无 macOS 14 或 Intel 实机验证 |
| macOS 安装包 | `.app`、`.dmg` 生成，ad-hoc 资源签名和 DMG 校验通过 | 本地签名，不是 Developer ID 签名或 Apple 公证发行 |

模型测试包含碎片 UTF-8／SSE、两种 API、完整工具参数、截断与断流拒绝、真实 usage、未知工具拒绝、nullable schema、图像探测及凭据脱敏。Agent 测试使用明确标识的 scripted HTTP 模型和真实文件／Shell／SQLite，覆盖 8K 首轮、批次拒绝、一次纠正、取消、48 步上限、压缩及丢失结果恢复。

远端测试使用真实 localhost WSS，覆盖证书 pin、一次性邀请、撤权、同 ID 去重、参数冲突、SQLite 迁移、崩溃后 `indeterminate`、输出游标／流式／限额／完整 artifact，以及跨配对和跨会话的桌面租约限制。它们不向真实桌面发送鼠标键盘输入。

## 0.3.0 外观与项目管理验收

新增外观选项卡、持久项目列表、显示名称重命名及保留文件的移除操作，使用说明见 [外观与项目管理](customization-0.3.0.md)。单独保存外观可在模型／终端运行时进行，不清当前对话；任务／终端运行时禁止项目切换与移除。项目和配置写入共用事务，已用真实 SQLite trigger 模拟配置写失败并验证完整回滚。

在实际打包应用中选择浅色和海蓝强调色、保存后重启，外观与测试项目名称均保留；从菜单移除专用项目后，原文件逐字保持，重启后该项目未重新出现。字体及系统主题变化的组合由 GUI 回归覆盖；原生手动检查使用默认字体。详细范围和校验值见 [本版原生验收](native-customization-0.3.0.json)。

## 0.2.0 基线升级验收

已实现的缺陷修复和依据见 [升级审查](upgrade-0.2.0.md)。合成微基准中，一万条旧活动记录恢复为 11 条模型消息，七次批量测量中位数从约 23.35 ms 降至 0.009 ms；图像预算计算无需复制 8 MiB base64。详细结果见 [性能探针](performance-upgrade-20261109.json)。这些结果不代表 Gemma 推理速度或任务质量提升。

本版原生 `.app` 显示版本 0.2；未请求模型时，性能面板显示无真实用量、吞吐为未知。真实终端在结束前显示起始标记，随后返回结束标记、`exit_code:0`、`output_complete:true`。磁盘 stdout 逐字匹配，工作目录与设置未改动；详见 [本版原生冒烟证据](native-gui-upgrade-0.2.0.json)。

统一回归曾发现远端测试断言输出分片必须为两个。由于系统分片和取消诊断不保证固定数量，改用启动次数、正文恰一次、唯一递增游标、客户端／日志一致及完整 artifact 字节一致验证；最终全部 10 项远端测试通过，未为此修改生产实现。

## 0.1.0 基线原生 GUI 验收与修复

用户解锁 Mac 后，直接在打包应用 `tauri://localhost` 中完成了以下验证：

- 设置中的工作目录可保存并在应用重启后恢复；原始配置在验收结束时恢复为未选择工作区。
- 文件列表可打开专用验收文件，编辑器显示无行号原文；中文修改保存后，磁盘内容逐字匹配，SHA256 为 `a62d902780d63644cdf336ddc0905c33a4e076a65d536af4ca0a6ac729bff802`，重启后可重新读取。
- 终端在进程退出前显示首段输出，正常结束返回退出码 0；GUI 停止按钮返回 `cancelled:true`，完整 stdout 仅有起始标记，没有执行后续标记。
- 设备预检实际找到捆绑 Node 与原生 helper。预检时浏览器缓存尚未安装、AX／录屏权限为 false，界面如实报告；没有授予新权限。
- 默认本机模型服务不可达时，界面显示实际连接拒绝错误，未显示模型或诊断成功。

原生测试发现并修复了两个问题：终端长输出没有自动跟随，以及切换工作区后保留旧编辑器。新增两项滚动回归和五项执行绑定回归，覆盖暂停／恢复跟随、重新打开、同路径切设备、迟到读取／保存及历史会话切换。修复已重新打包并在原生界面复验；工作区切换时旧编辑器和预览被清除，旧响应不能填回新目标。

详细观测、构建校验值和日志 artifact 见 [原生验收证据](native-gui-acceptance-20261109.json)。这些手动验证没有调用真实 Gemma 模型，也没有让 Harness 自动控制其他应用。

## 尚未通过的验收项

1. **用户 Windows LM Studio 实例不可达。**用户已明确回复“暂不可达”。尚未获取该实例版本、实际量化文件、加载上下文、GPU／显存、模板及 mmproj；真实模型工具选择正确率、编程任务成功率、首 token 与 tokens/s 均未知。
2. **macOS 原生桌面授权未完成。**实际 GUI 预检返回 Accessibility、Screen Recording 未授权；Harness 自身的真实 AX 输入、ScreenCaptureKit 截图、多显示器与撤权测试待执行。
3. **Windows／Linux 安装包与实机测试待执行。**已提供原生 CI 构建矩阵，但没有用户远端仓库的实际 CI 运行记录，当前交付目录不包含这两个系统的安装包。Ubuntu GNOME X11／Wayland、Ubuntu 26.04 GNOME Wayland、KDE Plasma 6 Wayland 均需对应交互会话验证。
4. **正式签名发行未完成。**未配置 Developer ID／公证或 Windows Authenticode 证书。

## 交付文件与复现

- macOS App：`target/release/bundle/macos/Gemma Harness.app`
- macOS DMG：`target/release/bundle/dmg/Gemma Harness_0.3.0_aarch64.dmg`
- 界面预览：运行 UI 测试生成 `tests/ui/artifacts/workbench.png`（浏览器预览，非原生屏幕截图；生成文件不纳入仓库）
- 结构化记录／SHA256：`docs/verification.json`
- 原版验证记录：`docs/verification-0.1.0.json`、`docs/verification-0.2.0.json`
- 构建与测试入口：[README](../README.md)、[Computer Use](computer-use.md)、[远端执行](remote-execution.md)

原生窗口验收已完成。Windows 实例恢复可达后，在 GUI 运行基础兼容性诊断，再执行真实编程任务；Harness 自身的原生桌面操作还需系统授权及单独验收。只有对应测试实际完成后，才更新其验收状态。
