# Third-party components

Project source is licensed under Apache-2.0. Dependencies retain their own licenses and notices; package lockfiles identify resolved versions.

| Component | Use | Upstream |
|---|---|---|
| Tauri, wry | Native GUI / WebView | https://github.com/tauri-apps/tauri |
| React, TypeScript, Vite | Workbench | https://github.com/facebook/react |
| Tokio, reqwest, rustls | Runtime and network | https://github.com/tokio-rs/tokio |
| SQLite / rusqlite | Durable local events | https://github.com/rusqlite/rusqlite |
| Playwright | Browser worker | https://github.com/microsoft/playwright |
| Node.js | Bundled browser worker runtime | https://github.com/nodejs/node |
| llama.cpp | Separately downloaded inference runtime | https://github.com/ggml-org/llama.cpp |
| windows-rs, windows-capture | Windows automation and capture | https://github.com/microsoft/windows-rs |
| atspi, ashpd, X11 / libei / PipeWire | Linux desktop integration | https://github.com/odilia-app/atspi |

The staging script preserves Playwright dependency licenses and copies the Node distribution LICENSE. Model weights and mmproj files are not included. Their Gemma/upstream and derivative distribution terms apply separately. Consult the exact model repository and Google Gemma terms before redistributing weights.

Codex (Apache-2.0) and DeepSeek Harness (MIT) were architectural references. Claude Code's public repository is a documentation/workflow reference under its commercial terms; its proprietary implementation was not reused.
