# Gemma 适配与验证

本文记录 Harness 对不同 Gemma 模型和推理后端的处理，以及可复现验证的方法。模型家族按名称自动识别只是提示；实际能力取决于具体权重、量化、模板、推理服务和视觉 projector，必须在实际部署上重新诊断。

## 配置与行为

- 「模型连接」可指定 Gemma 4、Gemma 3、FunctionGemma 或其他家族，以及 llama.cpp、LM Studio 或其他后端。使用管理的本地引擎时，自动模式可参考已读取的 GGUF 架构。更改与能力相关的连接配置或重启管理的引擎会使旧诊断失效；变更模型 ID 或重启管理的引擎会将模型系列和思考模式重置为自动，避免沿用旧权重的配置。
- 显式启用／关闭思考仅支持 Gemma 4 + llama.cpp + Chat Completions；自动模式不覆盖后端默认值。诊断单独报告是否在工具探测中观察到推理内容，未观察到不代表一定已关闭思考。输出 token 上限可配置，输入预算相应保留空间。
- FunctionGemma 的原生函数标记只在完整接收后解析，格式不完整、重复参数或不安全的控制标记会被拒绝。llama.cpp 所需的原生参数格式仅用于传输；内部和历史仍保存标准 JSON。结构化调用和原生调用不会混合执行。FunctionGemma 专门面向函数选择，不应当作通用编码 Agent；复杂参数、多步计划和最终答案应分别测试。
- 工具批次必须全部通过工具名称和原始 schema 校验才执行；可纠正的完整无效批次最多连续重试一次，中断或截断的流绝不执行。读取文件返回绑定文件版本的 `read_token`，可代替长 SHA256 用于修改；模型省略版本时，Harness 仅能补全本轮对同一路径实际读取到的令牌，并在事件中记录有效参数与来源。底层仍校验路径和文件哈希；令牌过期、文件变更或读取发生在上一轮时必须重新读取。
- `file_write` 省略版本时仅创建新文件，已有目标不会被覆盖；显式 `expected_sha256:null` 仍可用。新文件以不覆盖目标的原子链接方式发布，如果文件系统不支持这种方式会报错，不退回覆盖式写入。
- 大量文本通过 artifact 分页，偏移量为 UTF-8 字节位置，使用返回的 `nextOffset`；读取预览被截断时应跟随 `full_text_artifact`。命令输出超过 artifact 限额时明确报告不完整，不把有限内容描述为完整输出。
- 视觉输入要求图片输入及图片工具结果两项诊断通过。每次发送最多两张 PNG/JPEG、总计 8 MiB；单轮模型投影最多保留最近两张的像素，历史保存为本地 artifact 引用。外部推理服务可能在相同别名后切换模型，因此每轮首次发送图片前会重新验证视觉能力，这会增加延迟；失败时保留本地图片并停止传输。
- 临近输入预算时，文本请求可使用 llama.cpp 实际 `/apply-template` 与 `/tokenize` 计数，前提是 Chat Completions 后端能报告唯一且匹配的所选模型。带图请求、端点不支持或模型身份不明确时，回退到带视觉余量且标记为估算的计数。远端服务报告的模型 ID 与实际权重哈希并非同一证据。

## 复现真实模型测试

先自行启动目标推理服务，按实际情况修改下列变量；测试会向该服务发送合成指令和图片，不需要用户文件。工具参数评测不执行工具；Agent 评测仅在全新临时工作区执行文件操作，并禁用终端、浏览器、桌面和 MCP。诊断使用合成彩色图片，只证明本次探测，不证明所有图片和任务都可用。

```sh
export HARNESS_EVAL_URL=http://127.0.0.1:8080
export HARNESS_EVAL_MODEL=your-loaded-model-id
export HARNESS_EVAL_BACKEND=llama_cpp  # auto、llama_cpp、lmstudio、other
export HARNESS_EVAL_FAMILY=gemma4      # auto、gemma4、gemma3、functiongemma、other
export HARNESS_EVAL_THINKING=off       # auto；Gemma 4 + llama.cpp 可用 on/off
export HARNESS_EVAL_REPEATS=3

cargo run --release -p harness-model --example gemma_tool_eval > tool-results.json
cargo run --release -p harness-core --example gemma_agent_eval > agent-results.json
cargo run --release -p harness-model --example gemma_diagnose > diagnosis.json
```

如服务需要认证，可通过 `HARNESS_EVAL_API_KEY` 注入密钥；不要把密钥或含敏感请求的输出提交到仓库。工具评测每项输出参数、耗时和通过情况；运行次数范围为 1–10，输出通常受后端默认采样影响，不应将少量样本当作统计精度。诊断可能发现模型不支持视觉或工具回灌，这是实测结果而不是自动修复。

本次所用模型文件的固定 revision、SHA256、后端版本和原始合成测试结果见 [机器可读验证记录](gemma-adaptation-verification-20261109.json)。项目首选的 Gemma 4 26B Heretic 权重及其实际部署环境并未因此自动通过认证；应在目标机器上按上述命令和 GUI 诊断复测。

## 2026-11-09 实测摘要

测试运行于 macOS arm64、16 GiB 内存、llama.cpp b12211、8K 上下文和单推理槽。模型文件和视觉 projector 均在本机计算 SHA256；服务的模型别名本身不能证明权重身份。

- [Gemma 4 E2B Q4_0](https://huggingface.co/ggml-org/gemma-4-E2B-it-GGUF/tree/main)：固定的 11 项工具评测，思考开／关各 11/11。关闭思考的三轮隔离 Agent 测试共 8/12：修改和查找各 3/3，创建 2/3，精确读取 0/3。开启思考的单轮测试为 3/4，失败项是写入时丢失原文件末尾换行。另一次关闭思考的单轮测试为 2/4。文件状态正确不保证模型的中间陈述正确。
- Gemma 4 视觉诊断中，关闭思考首次未通过图片输入、因此图片工具结果未测；随后三次输入与工具结果均通过。开启思考的两次诊断均通过，并观察到工具探测中的推理内容。诊断只覆盖随机色块和工具图片回灌，结果有样本波动。
- [FunctionGemma 270M Q8_0](https://huggingface.co/ggml-org/functiongemma-270m-it-GGUF)：三轮固定工具评测 24/33；嵌套参数、`null` 类型和不调用工具的反例各 0/3。单轮隔离 Agent 任务 0/4。该模型针对函数调用任务，实际复杂任务需另行评估或针对任务微调，不能把语法适配等同于通用 Agent 能力。参见 [Google 的模型卡](https://huggingface.co/google/functiongemma-270m-it)。

这些是少量合成样本，后端使用默认采样，不能作为统计准确率或其他 Gemma 权重的认证。真实 LM Studio 的 FunctionGemma 行为、Windows／Linux 安装包和本次新包的原生 GUI 人工验收尚未测试。
