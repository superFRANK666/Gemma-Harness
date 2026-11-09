# Harness workers

These workers perform real browser and native desktop actions on the selected execution device, which is local by default. Selecting a LAN model alone never transfers computer control to the model server; a remote execution device must be selected separately.

Build the native helper on each OS with `node workers/build-native.mjs`. The resulting executable is placed under `workers/native/bin/<macos|windows|linux>/`. Windows/Linux require Rust; macOS requires Xcode Command Line Tools for the build. Packaged applications should bundle the compiled helper, not invoke a compiler at runtime. `HARNESS_DESKTOP_HELPER` overrides the executable; `HARNESS_RESOURCE_DIR` points at a resource directory containing `workers` or directly at `workers`.

The JSONL interface is `{id,method,params}` → `{id,result}` or `{id,error:{message}}`. Methods are `capabilities`, `observe`, `screenshot`, `act`, `cancel`. Snapshots have fresh IDs; using an expired ID fails. A successful input API call reports `requires_observation:true`: the agent must inspect the resulting application state, not assume a task succeeded. No worker fabricates observations or screenshots.

## Browser

Packaged applications bundle Node and Playwright; the GUI's browser setup action downloads Chromium into application data without requiring a user terminal. `Executor::setup_browser` and worker startup share `PLAYWRIGHT_BROWSERS_PATH=<app-data>/browser-cache`. Bundled Node lives at `workers/browser/bin/node` (`node.exe` on Windows); `HARNESS_NODE` explicitly overrides it, and a system Node is a developer fallback.

For development only, run `npm ci --prefix workers/browser`, then `npm run install-browser --prefix workers/browser`. Chromium runs visibly by default with an isolated persistent profile for each workspace. `HARNESS_BROWSER_HEADLESS=1` is for tests. `HARNESS_BROWSER_PROFILE` selects a dedicated profile; do not point it at a browser profile already in use.

Observations combine Playwright's accessibility snapshot with bounded live element handles. Coordinate clicks require a fresh screenshot, semantic actions require a fresh element ref. A new navigation/observation/action invalidates prior refs. Screenshot payloads contain an actual PNG and an artifact path. Tests: `npm test --prefix workers/browser`.

## Native platform coverage

- **macOS 14+:** Swift helper uses AXUIElement for semantic controls, ScreenCaptureKit for actual PNG captures, and CGEvent for input. The OS must grant Accessibility and Screen Recording permissions to the signed app/helper. Secure text values are redacted. Launch takes an application bundle ID or absolute `.app` path.
- **Windows 11:** Rust helper uses UI Automation COM and SendInput. Windows.Graphics.Capture is the primary screenshot backend for a chosen native window or the primary monitor, with a bounded first-frame wait and explicit session cleanup. Monitor-capture failure falls back to a real GDI visible virtual desktop screenshot; every result reports `capture_backend` and a fallback reason. A requested window is never silently substituted with another display. Ordinary-user integrity restrictions apply; UAC secure desktop, elevated targets, protected capture content and locked sessions are not supported. Applications with no Value/Invoke pattern require a current screenshot for coordinate interaction. Launch takes an executable path/name without shell arguments; it reports a process ID, not an assumed application state. WGC code can be cross-checked here, but requires Windows hardware/VM acceptance before claiming runtime coverage.
- **Linux X11:** Rust uses AT-SPI2 over D-Bus, XTEST input and real X11 screen capture. A live DISPLAY and the accessibility bus are required. Capture supports the common 32-bit little-endian pixel layout.
- **Linux Wayland:** semantic actions use AT-SPI2; pointer/keyboard control uses a user-authorized `org.freedesktop.portal.RemoteDesktop` session with ScreenCast and PipeWire. Actual frame capture additionally requires `gst-launch-1.0`, `pipewiresrc`, `videoconvert`, and `pngenc`. On Ubuntu these come from `gstreamer1.0-tools`, `gstreamer1.0-pipewire`, `gstreamer1.0-plugins-base`, and `gstreamer1.0-plugins-good`. GNOME/KDE portal implementations are the supported path. A screenshot-only wlr portal cannot authorize desktop input. No root/uinput bypass is used.

The native helper has no listening network port. The executor serializes native actions across its instances and terminates the worker process group on cancellation/timeout. Key combinations send matching release events immediately; workers do not retain held modifier keys across calls.

Permission and dependency failures return explicit errors. Capability probes do not prompt for permission. An actual macOS accessibility/capture action or Wayland desktop-sharing request can open the system's authorization dialog; the user must grant it. The helper never changes desktop portal configuration or grants itself OS permissions.
