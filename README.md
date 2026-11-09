# Gemma Harness

面向本地 Gemma 模型的桌面编程与电脑操作工作台。Rust Agent 核心、Tauri 2 GUI、React/TypeScript；模型推理与工具执行可以部署在不同设备。

当前开发版 **0.3.0**：新增可预览、保存的主题／字体／配色设置，以及项目列表的重命名、移除；使用说明见 [外观与项目管理](docs/customization-0.3.0.md)。此前的推理与上下文改进见 [0.2.0 升级审查](docs/upgrade-0.2.0.md)。

首个适配配置是 [Gemma 4 26B A4B Instruct Ultra Uncensored Heretic I1 GGUF](https://huggingface.co/mradermacher/gemma-4-26B-A4B-it-ultra-uncensored-heretic-i1-GGUF)，固定模型仓库 revision `9cada68ea11a8f361e4b16a7a97e53d99b0918c0`。具体量化、模板、推理服务和视觉 projector 仍须使用真实部署诊断；通用协议测试不等于这个模型已经认证。

## 下载安装

在 [GitHub Releases](https://github.com/superFRANK666/Gemma-Harness/releases) 选择预发布版本，并按设备下载：

| 设备 | 安装包 |
| --- | --- |
| macOS 14+，Apple Silicon | `macOS-arm64.dmg` |
| macOS 14+，Intel | `macOS-x64.dmg` |
| Windows x64 | `Windows-x64-setup.exe` 或 `Windows-x64.msi` |
| Linux x64 | Debian／Ubuntu 使用 `.deb`；其他兼容环境可尝试 `.AppImage` |

文件名前缀包含应用名称和版本，`SHA256SUMS` 可用于校验下载。macOS 包未经 Apple Developer ID 签名或公证，Windows 包未经 Authenticode 签名，系统可能显示安全提示。Windows、Linux 和 Intel Mac 尚需实际设备上的安装及原生桌面操作验收；支持的平台与依赖见 [Computer Use](docs/computer-use.md)。

## 使用桌面应用

1. 打开应用，进入「模型连接」，填写推理服务地址，点击「发现模型」，选择实际加载实例。可连接本机、局域网或远程 HTTP(S) API。跨机器连接 LM Studio 时，需先在服务端启用网络监听，并填写实际地址及认证；模型 API 地址与执行设备 WSS 地址分别配置。按实际情况选择 Gemma 家族和后端；自动识别仅根据名称推测，不能证明实际权重和模板。
2. 选择项目目录。在「执行设备」中可选择本机或已配对设备；远端使用其明确共享的目录。
3. 点击「保存并运行诊断」。它会实际测试文本、函数回灌、图片输入和图片工具结果；失败或未知不会显示为已通过。
4. 在工作台输入任务，也可添加最多两张 PNG／JPEG 图片（总计不超过 8 MiB）。图片只有在视觉诊断通过后才会发往模型；单轮投影最多保留最近两张图片的像素，历史使用本地 artifact 引用。文件变更使用读取凭据或 SHA256 校验版本，工具执行和模型输出流式显示，会话保存在本机 SQLite；本机／远端命令在结束前即可看到输出，并能独立停止。切换工作区或设备会关闭当前编辑器并清除检查区；未保存的编辑内容不跨绑定保留，切换前请先保存。
5. 浏览器首次使用，在「工具与扩展」点击「安装浏览器组件」。原生桌面还需系统授权，详见 [Computer Use](docs/computer-use.md)。

「设置与连接 → 外观」提供浅色／深色／跟随系统、强调色、界面与代码字体／字号。项目旁的「…」可重命名或移除；重命名只改显示名，移除保留文件、Git 仓库和历史会话。

「本地引擎」可从固定清单安装 SHA256 校验的 llama.cpp **b12211**，选择 GGUF 和匹配的 mmproj 后启动。默认 8K 上下文、单推理槽、仅监听本机；是否适合更长上下文由硬件与实际测试决定。权重不随应用打包，不自动更换模型或调用云端模型。

Gemma 4 的显式思考开关目前仅支持 llama.cpp 的 Chat Completions；其他后端使用「自动」并遵循其模板。可配置输出 token 上限。FunctionGemma 侧重函数选择，不应视为通用编程模型；它的参数准确率和多步任务效果需要按实际模型测试。细节、可复现评测和已知局限见 [Gemma 适配与验证](docs/gemma-adaptation.md)。

「执行设备」可让另一台电脑安装同一应用、共享一个工作目录并生成五分钟有效的一次性配对串。连接使用证书 pin 和可撤销凭据；首版使用局域网、VPN 或可达地址，不提供公网中继。详见 [远端执行](docs/remote-execution.md)。

## 从源码运行

需要当前 stable Rust、Node.js 24、平台原生开发工具。macOS 使用 Command Line Tools；Windows 使用 Visual Studio C++ Build Tools 与 WebView2；Linux 需要 WebKitGTK 4.1、GTK 3、DBus 和相应桌面依赖。

```sh
npm ci
npm run setup:workers
npm run desktop
```

`setup:workers` 编译原生 helper、安装固定的 Playwright 依赖、从 Node 官方发布地址下载并校验嵌入式 Node，然后将分发资源放入 `build/worker-resources/`。已下载的 Node 压缩包在 `build/downloads/` 缓存，每次使用前重新校验 SHA256；重新准备资源时会替换旧平台资源。不需要最终用户安装 Node 或使用终端。

若 Rust 不在 PATH 中，将 Rust 的 bin 目录加入当前构建环境，或给 `workers/build-native.mjs` 设置 `CARGO` 指向 cargo 可执行文件。npm 缓存权限异常时，可用 `npm ci --cache /tmp/harness-npm-cache`。

```sh
npm run desktop:build
```

安装包输出到 `target/release/bundle/`。macOS `.app/.dmg`、Windows `.exe/.msi`、Linux `.deb/.AppImage` 分别需要原生构建环境。GitHub Actions 分别运行三平台验证和四种架构打包；发布流程只有在所有安装包齐全并通过上传校验后才公开预发布版本，详见 [发布流程](docs/releasing.md)。macOS 打包使用本地 ad-hoc 签名并校验资源，DMG 不依赖 Finder 自动化授权。公开 Developer ID 签名／公证仍需要相应证书，不能将本地签名当作公证发行。

## 验证

```sh
cargo test --workspace --exclude harness-desktop
cargo check -p harness-desktop
npm run build
npx playwright install chromium
npm run test:ui
npm --prefix workers/browser test
npm run test:release
```

真实 Chromium 集成与安装测试是额外的网络／图形测试，不作为离线单元测试冒充通过：

```sh
HARNESS_BROWSER_INTEGRATION=1 npm --prefix workers/browser test
cargo test -p harness-executor -- --ignored
```

测试中的 scripted model 明确是测试夹具；产品没有模拟模型或合成成功响应。当前实测边界与结果见 [验收记录](docs/acceptance.md)。

## 结构

```text
apps/desktop/          Tauri 宿主与 React 工作台
crates/harness-model/  协议、SSE、Gemma schema、模型诊断与 profile
crates/harness-core/   Agent 循环、上下文、SQLite、引擎与 MCP
crates/harness-executor/ 文件、命令、Git、浏览器及桌面执行
crates/harness-remote/ WSS、配对、操作日志、去重与桌面租约
workers/              Playwright 与各平台原生 helper
```

模型工具调用必须等流结束并通过完整批次校验后才执行。上下文采用保留原日志的投影压缩；图片二进制不计作文本 token。本地 GGUF 元数据／模板指纹会单独保存在 model-profiles，完整权重与远端 runtime 身份未验证时仍标为未知。临近预算时，符合条件的 llama.cpp 文本请求会使用其模板和 tokenizer 计数；不支持、模型不明确或含图片时回退到明确标识的保守估算。

推理额度按服务 origin 共享；对话、摘要和诊断串行生成，工具运行期间释放额度。性能面板显示真实用量和含预填充／网络的请求吞吐，无用量则不虚构速度。大段文本通过 artifact 分页，重复失败三次会停止本轮；本地或远端动作回执丢失后显示“结果待确认”，先观察再决定是否重试。

参考实现：[Codex](https://github.com/openai/codex)、[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)、[Claude Code 公开资料](https://github.com/anthropics/claude-code)。本项目没有复制 Claude Code 的商业核心。
