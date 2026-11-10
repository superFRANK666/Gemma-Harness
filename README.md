# Gemma Harness

面向本地 Gemma 模型的桌面编程与电脑操作工作台。模型推理和工具执行可以分别放在本机或其他设备上。

![Gemma Harness 浅色模式主页](docs/images/home-light.png)

*浅色模式主页，空工作区界面预览。*

## 主要功能

- **编程工作台**：对话中读取和修改项目文件、搜索代码、运行命令，并查看工具执行过程与 Git 差异。
- **电脑与浏览器操作**：通过浏览器和原生桌面工具观察界面、执行操作；可添加图片供具备视觉能力的模型使用。
- **灵活连接**：连接 LM Studio、llama.cpp 或兼容的推理服务，也可运行本地 GGUF 模型；支持将任务发送到已配对的执行设备。
- **本地工作区**：管理项目与会话，在本地保存历史，并调整主题、配色和字体。

## Gemma 适配性

[OpenCode](https://opencode.ai/docs/models/) 和 [Aider](https://aider.chat/docs/llms/ollama.html) 等通用编程工具也能连接本地模型；OpenCode 的文档还列出了 Gemma 示例。Gemma Harness 将 Gemma 作为首要适配目标，提供针对模型系列和推理后端的专门处理：

- 支持选择 Gemma 4、Gemma 3 和 FunctionGemma 系列；针对 FunctionGemma 处理其原生函数调用格式，并在执行前校验工具参数。
- 在 Gemma 4 与 llama.cpp 的 Chat Completions 组合下提供思考模式控制；针对图片输入和图片工具结果分别运行诊断，通过后才发送图片。
- 针对工具调用、上下文预算及模型输出提供检查和恢复机制，并保留任务过程和原始会话历史。

这些适配不等于所有 Gemma 权重和后端都已验证，也不代表任务效果一定优于其他工具：目前没有同条件的横向对照评测。模型质量、量化、模板和运行环境都会影响结果；实测范围和局限见 [Gemma 适配与验证](docs/gemma-adaptation.md)。
