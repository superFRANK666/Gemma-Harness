# Computer Use：实现、安装与验收

Computer Use 在**当前会话选定的执行设备**上运行。默认设备是本机；仅修改模型 API 地址不会把鼠标、文件或浏览器操作转移到模型服务器。远程执行设备须单独配对、选择，其权限和运行依赖也须在该设备上满足。

这份文档描述当前代码提供的能力，不把编译通过等同于真实桌面验收通过。

## 当前支持矩阵

| 环境 | 语义观察与操作 | 实际截图 | 边界 |
| --- | --- | --- | --- |
| macOS 14+，Apple Silicon／Intel 构建目标 | AXUIElement 无障碍树；AX press／value；CGEvent 键鼠 | ScreenCaptureKit，主显示器或指定窗口 | 需要辅助功能、录屏权限；受保护内容与未暴露无障碍控件的界面不能保证可操作 |
| Windows 11 x64 | UI Automation control view；Invoke／Value pattern；SendInput | 优先 Windows.Graphics.Capture：主显示器或指定 HWND；显示器捕获失败时提供明确标记的 GDI 可见虚拟桌面后备 | 普通交互会话；UAC 安全桌面、锁屏、较高权限目标及受保护捕获内容不在支持范围 |
| Linux X11，x64 | AT-SPI2 D-Bus；XTEST 键鼠 | X11 根窗口，当前支持常见的 32 位、小端像素布局 | 需要可访问的 DISPLAY、XTEST 扩展和 AT-SPI 总线 |
| Linux Wayland，x64 | AT-SPI2 语义操作；RemoteDesktop portal 键鼠 | ScreenCast portal／PipeWire，经 GStreamer 取得实际 PNG | 需要支持 RemoteDesktop 的桌面后端、系统授权及下列运行依赖；当前选取一块显示器，不支持指定 `window_id` 捕获 |
| 受管 Chromium，以上桌面平台 | Playwright ARIA 快照及实际元素句柄 | Chromium 视口 PNG | 使用专用持久配置目录；不直接接管用户已经打开的任意浏览器配置目录 |

Linux 的验收基线应使用 Ubuntu 24.04 LTS GNOME 的 X11／Wayland 会话，并分别验证；Ubuntu 26.04 LTS GNOME 以 Wayland 会话验证。首版待认证矩阵还包括 KDE Plasma 6 Wayland；其他桌面通过运行时能力探测接入。这里的基线是需要执行的验收范围，**尚不是已完成的运行验证记录**。只有 Screenshot／ScreenCast 的 wlr portal 不能授权原生桌面输入；XWayland 也不能替代对原生 Wayland 窗口的输入授权。

## GUI 使用与系统权限

先在应用设置中启用需要的工具类别。桌面工具默认关闭；启用应用内开关不会代替操作系统授权。能力检测只进行预检，不弹出授权对话框。

**macOS：**在“系统设置 → 隐私与安全性 → 辅助功能”允许 Harness／原生 helper；在“屏幕录制”或新版系统的“屏幕与系统音频录制”中允许捕获。第一次实际无障碍操作或截图请求可触发系统授权提示。授权或安装签名发生变化后可能需要退出并重新打开应用／helper。当前代码不使用 AppleScript 控制其他应用，Apple Events 权限不是 AX／CGEvent 操作的替代条件。

**Windows：**在已经登录、未锁定的用户桌面运行。普通用户的 SendInput 受 UIPI 限制，无法借此控制更高完整性级别的应用；应用不自动提权。Windows.Graphics.Capture 保持系统默认捕获边框行为。API 调用返回成功仍需重新观察目标应用状态。

**Linux X11：**应用与目标程序应位于同一图形用户会话，并能访问 DISPLAY 和桌面无障碍总线。AT-SPI 不可用时语义观察会报错；不会用虚构控件补齐界面。

**Linux Wayland：**系统的共享对话框需批准所选显示器及键盘／指针控制。当前截图实现创建一个结合 RemoteDesktop 和 ScreenCast 的会话，因此截图准备也可能显示输入控制授权选项。用户拒绝、撤销授权、桌面后端缺少接口或会话关闭时，操作应停止并返回错误。helper 不修改 portal 配置，不通过 root／uinput 绕过授权。

## Linux 运行依赖与安装包

Debian／Ubuntu 的完整桌面能力需要以下运行组件；它们与编译用的 `*-dev` 包不同：

| 用途 | 包或系统条件 |
| --- | --- |
| Tauri GUI | `libwebkit2gtk-4.1-0`、`libgtk-3-0`、`libayatana-appindicator3-1`，由发行版提供对应兼容包 |
| 无障碍树 | `at-spi2-core`、可工作的用户 D-Bus 会话，目标应用需暴露 AT-SPI 控件 |
| Wayland 授权 | `xdg-desktop-portal`，以及与当前桌面匹配、实现 RemoteDesktop／ScreenCast 的后端，例如 `xdg-desktop-portal-gnome` 或 `xdg-desktop-portal-kde` |
| Wayland 图像流 | `pipewire` 及正常工作的桌面 PipeWire 会话 |
| PNG 捕获管线 | `gstreamer1.0-tools`、`gstreamer1.0-pipewire`、`gstreamer1.0-plugins-base`、`gstreamer1.0-plugins-good`，分别提供 `gst-launch-1.0`、`pipewiresrc`、`videoconvert`、`pngenc` |
| Linux 应用启动动作 | `libgtk-3-bin` 中的 `gtk-launch`；`app` 应为已安装应用的 desktop 文件 ID |
| X11 输入 | 已运行的 X server／DISPLAY 和 XTEST 扩展；helper 通过 Rust X11 协议实现访问，不调用 xdotool |

`.deb` 的完整桌面发行应声明适用的运行依赖，而不能只声明 WebKit／GTK。portal 后端必须与用户正在运行的桌面匹配，不能以任意安装另一个后端来保证控制能力。AppImage 内置应用资源也不代表它内置了用户的合成器、D-Bus、PipeWire 服务或 portal 授权；启动前仍需进行能力检测。

Linux 的 `capabilities` 会预检 AT-SPI、X11 连接、portal 支持及 GStreamer 命令是否存在。具体插件和真实图像流还会在第一次捕获时检查，因此预检中的 `screen_capture:true` 不能代替实际截图验收。

## 浏览器运行时与资源打包

安装包包含 Node 和固定版本 Playwright。用户在 GUI 触发浏览器安装／准备操作即可下载 Chromium，不需要自行安装 Node 或运行命令行。底层为 `Executor::setup_browser`；当前 GUI 命令名为 `browser_setup`。远程执行设备应在其本机 GUI 完成浏览器准备。

资源布局如下：

```text
workers/
  browser/bin/node                 # Windows 为 node.exe
  browser/worker.mjs
  browser/node_modules/playwright/
  browser/node_modules/playwright-core/
  native/bin/<macos|windows|linux>/harness-desktop[.exe]
```

`scripts/setup-workers.mjs` 根据 `assets/node-runtime.json` 下载并校验固定 Node 分发包，复制原生 helper、Node、Playwright 实体目录和 Node 许可证到暂存资源；不依靠 `node_modules/.bin` 符号链接。Tauri 将暂存的 `workers` 目录映射到应用资源目录。原生 helper 必须在各自平台构建；终端用户不应在启动应用时编译 helper。

浏览器准备和实际 worker 使用同一个 `PLAYWRIGHT_BROWSERS_PATH=<应用数据目录>/browser-cache`。每个工作区有独立的持久浏览器配置目录；默认有可见窗口。Chromium 下载可能需要数分钟，可取消。`capabilities.browser` 报告捆绑 Node、Playwright 资源、实际 Chromium 可执行文件和缓存位置；未完成准备时不会报告浏览器已就绪。

开发／诊断覆盖项为 `HARNESS_RESOURCE_DIR`、`HARNESS_NODE`、`HARNESS_DESKTOP_HELPER`、`HARNESS_BROWSER_PROFILE`。没有捆绑 Node 时才尝试系统 Node 开发后备；发行包应验证 `bundled_node:true`。`HARNESS_BROWSER_HEADLESS=1` 用于自动化测试，不是正常桌面使用的默认值。

## 工具接口与状态约束

模型工具为 `browser_observe`、`browser_screenshot`、`browser_act`、`desktop_observe`、`desktop_screenshot`、`desktop_act`。GUI 通过内部命令执行能力探测与浏览器准备。内部 worker 使用 JSONL：

```json
{"id":"request-1","method":"observe","params":{}}
{"id":"request-1","result":{"snapshot_id":"...","elements":[]}}
```

失败返回同一请求 ID 和 `error.message`。内部方法包括 `capabilities`、`observe`、`screenshot`、`act`、`cancel`。

- 语义操作使用最近观察返回的 `snapshot_id` 与 `ref`。新观察、截图或动作后应重新取得引用；过期 ID 会被拒绝。原生坐标点击只接受当前截图建立的状态，不能拿历史图像猜测位置。
- 浏览器动作支持 `click`、`fill`、`press`、`scroll`、`back`、`forward`、`new_tab`、`close_tab`。导航只接受 HTTP／HTTPS 和 `about:blank`。ARIA 文本与元素列表有输出上限，密码输入值在返回快照前脱敏。
- 原生动作支持语义 `click`／`fill`、`key` 组合键、`scroll`、`launch`；控件必须实际提供平台所需的 press／Invoke／Value／EditableText 能力。`launch` 的应用标识分别为 macOS bundle ID 或绝对 `.app` 路径、Windows 可执行文件路径／名称、Linux desktop 文件 ID，不把 `app` 当作 shell 命令解析。
- `desktop_observe` 的 `app` 过滤按平台实现：macOS 可匹配应用名称／bundle ID／PID，Windows 匹配顶层窗口标题，Linux 匹配 AT-SPI 应用名称。窗口截图的 `window_id` 当前由 macOS／Windows 支持；Linux 截图以显示器为单位。
- 截图结果包含实际 PNG 的 `content` 图像项、磁盘 `artifact` 和 `snapshot_id`。Windows 同时报告 `capture_backend`；WGC 失败后的显示器后备还报告 `fallback_reason`，指定窗口失败不会静默换成其他显示器。
- 原生动作成功只表示输入／无障碍 API 已接受调用，返回 `requires_observation:true`；任务完成必须由后续观察证明。按键组合立即发送对应释放事件，不跨调用保存按住状态。
- 同一 Harness 进程的 Executor 共享原生桌面锁；远程设备的会话租约由远程执行层管理。取消／超时会关闭浏览器并终止 worker 进程树，后续需重新观察。此锁不阻止真人同时操作鼠标，也不跨多个独立 Harness 应用实例提供操作系统级独占。

## 真实设备验收方法

以下验收应在实际安装包、正常交互用户会话中执行，并保存截图／工具结果。需要模型参与时，先连接准备验收的确切模型与推理服务；通过工具层测试不能直接推导出模型在长任务中的可靠性。

1. **浏览器安装与实际操作：**在一个尚无 Chromium 缓存的应用数据目录启动安装包，从 GUI 准备浏览器，确认能力报告中的捆绑 Node 和 Chromium 路径。打开可控测试页面，观察按钮，使用其当前引用点击，重新观察确认页面文字改变；获取截图，确认 GUI 图像与实际页面一致、PNG artifact 可打开。关闭网络后重复已缓存浏览器的启动。
2. **原生语义操作：**开启桌面工具，在 macOS 使用 TextEdit、Windows 使用记事本、Linux 使用已安装且暴露 AT-SPI 的文本编辑器。只用桌面工具观察、选中可编辑控件并输入一段标记文字，重新观察并截屏确认。测试保存时只使用专用验收工作区中的新文件，不操作现有业务文档。
3. **截图和坐标：**从当前截图选择明确的测试控件点击，确认目标窗口和坐标空间正确。分别测试缩放、第二显示器、窗口移动；Windows 另测指定 HWND 的 WGC 捕获，以及明确标记的 GDI 后备。不能以返回了 PNG 文件代替内容和坐标正确性检查。
4. **系统授权：**macOS 分别在未授权、授权、撤回权限后检查；Wayland 取消、批准、撤销共享会话。拒绝时应显示具体错误，不应继续输入或产生替代截图。Windows 测试普通目标与较高权限目标，确认限制得到如实报告。
5. **状态恢复与停止：**执行一次动作后故意复用旧 `snapshot_id`，应被拒绝。页面导航／控件移除后重取引用。启动一个可控的慢页面操作并按 GUI 停止，确认没有后续自动点击，重新开始会话可以恢复。
6. **Linux 分会话验收：**在 X11 与 GNOME／KDE Wayland 中分别记录会话类型、能力结果、真实截图和输入结果。仅在 Xvfb、浏览器或一种合成器上通过，不能写成所有 Linux 桌面通过。

## 当前已验证内容（2026-11-09）

- macOS 原生工作台：0.1.0 通过真实 `.app` 的配置、文件读写、终端流式／停止及重启验收；0.2.0 另通过新版本启动、性能面板未知状态和真实终端完整输出，详见 [验收记录](acceptance.md)。此项由 CUA 操作 Harness 界面，不代表 Harness 自身的桌面自动化已获授权。
- macOS 本机：原生 Swift helper 编译通过；无提示能力探测实际返回 `accessibility:false`、`screen_capture:false`。尚未完成授予权限后的原生桌面截图、输入及多显示器验收。
- 浏览器：真实无头 Chromium 本地测试页面完成观察、点击并验证文字变化、旧引用拒绝、真实 PNG／artifact 一致性、取消，以及密码快照脱敏回归，共两项测试通过。
- 受管安装：`Executor::setup_browser` 从空的临时应用数据缓存实际下载 Chromium，并经 Rust JSONL bridge 观察页面、返回与 artifact 一致的实际 PNG；该独立集成测试通过，耗时约 400 秒，主要为下载。
- Executor：11 项常规测试通过，覆盖文件边界、哈希编辑、备份、进程取消、流式 UTF-8／限额／artifact，以及有界 worker 交换、故障回收、已发送动作结果待确认和输出捕获错误；真实 Rust→Chromium 集成另用现有缓存显式通过。
- Windows x64：原生 helper（包括 WGC 首选捕获及 GDI 后备）和 Executor 的交叉 `cargo check` 通过。**未进行 Windows 真机／虚拟机原生桌面运行验收。**
- Linux x64：原生 helper 的交叉 `cargo check` 通过。**未进行 Linux X11／Wayland 的实际 portal、PipeWire、AT-SPI、截图或输入验收。**

复现开发测试可使用：

```sh
cargo test -p harness-executor
HARNESS_BROWSER_INTEGRATION=1 npm --prefix workers/browser test
HARNESS_BROWSER_HEADLESS=1 cargo test -p harness-executor browser_managed_setup_and_bridge -- --ignored
cargo check --manifest-path workers/native/Cargo.toml --target x86_64-pc-windows-gnu
cargo check --manifest-path workers/native/Cargo.toml --target x86_64-unknown-linux-gnu
```

这些命令面向维护者。`browser_managed_setup_and_bridge` 默认忽略，需显式运行，会下载真实浏览器；默认 `npm test` 也不会自动执行需要 Chromium 的浏览器集成用例。仓库中的跨平台构建流程只有在实际运行并检查结果后，才可作为相应构建的证据；它不替代以上真实桌面验收。

接口参考：[Apple ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)、[Microsoft UI Automation](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-uiautomationoverview)、[SendInput／UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)、[XDG RemoteDesktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)、[XDG ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)、[AT-SPI2](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/)、[windows-capture 源码](https://github.com/NiiightmareXD/windows-capture)。这些资料说明底层接口；当前实现和已执行验证以本文记录及仓库代码为准。
