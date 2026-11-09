use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, io::Cursor};
use windows::{
    core::{Interface, BSTR},
    Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::*,
        System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
        },
        UI::{Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};

pub struct Desktop {
    automation: IUIAutomation,
    snapshot: String,
    refs: HashMap<String, IUIAutomationElement>,
    screenshot: bool,
    origin: (i32, i32),
    size: (i32, i32),
}
impl Desktop {
    pub fn new() -> Result<Self> {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            let automation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            Ok(Self {
                automation,
                snapshot: String::new(),
                refs: HashMap::new(),
                screenshot: false,
                origin: (0, 0),
                size: (1, 1),
            })
        }
    }
    pub async fn handle(&mut self, method: &str, args: &Value) -> Result<Value> {
        match method {
            "observe" => self.observe(args),
            "screenshot" => self.capture(args),
            "act" => self.act(args),
            "cancel" => {
                self.snapshot.clear();
                self.refs.clear();
                Ok(json!({"cancelled":true}))
            }
            "capabilities" => Ok(
                json!({"platform":"windows","accessibility":"UI Automation COM","capture":"Windows.Graphics.Capture monitor/window; explicit GDI virtual desktop fallback","wgc_supported":windows::Graphics::Capture::GraphicsCaptureSession::IsSupported().unwrap_or(false),"input":"SendInput; equal or lower integrity applications only","secure_desktop":false}),
            ),
            _ => bail!("Unknown native method"),
        }
    }
    fn observe(&mut self, args: &Value) -> Result<Value> {
        unsafe {
            self.snapshot = uuid::Uuid::new_v4().to_string();
            self.refs.clear();
            self.screenshot = false;
            let root = self.automation.GetRootElement()?;
            let walker = self.automation.ControlViewWalker()?;
            let max_depth = args["depth"].as_u64().unwrap_or(4).clamp(1, 8);
            let mut nodes = Vec::new();
            let mut queue = vec![(root, 0u64)];
            while let Some((element, depth)) = queue.pop() {
                if nodes.len() >= 200 {
                    break;
                }
                let name = element
                    .CurrentName()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if depth == 1
                    && args["app"]
                        .as_str()
                        .is_some_and(|filter| !name.to_lowercase().contains(&filter.to_lowercase()))
                {
                    continue;
                }
                let reference = format!("e{}", nodes.len() + 1);
                self.refs.insert(reference.clone(), element.clone());
                let role = element
                    .CurrentLocalizedControlType()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let rect = element.CurrentBoundingRectangle().unwrap_or_default();
                nodes.push(json!({"ref":reference,"name":name.chars().take(180).collect::<String>(),"role":role,"depth":depth,"enabled":element.CurrentIsEnabled().map(|v|v.as_bool()).unwrap_or(false),"bounds":{"x":rect.left,"y":rect.top,"width":rect.right-rect.left,"height":rect.bottom-rect.top},"process_id":element.CurrentProcessId().unwrap_or(0),"window_id":element.CurrentNativeWindowHandle().map(|hwnd|(hwnd.0 as usize).to_string()).unwrap_or_default()}));
                if depth < max_depth {
                    let mut children = Vec::new();
                    let mut next = walker.GetFirstChildElement(&element);
                    while let Ok(child) = next {
                        next = walker.GetNextSiblingElement(&child);
                        children.push((child, depth + 1));
                        if children.len() >= 200 {
                            break;
                        }
                    }
                    queue.extend(children.into_iter().rev());
                }
            }
            Ok(
                json!({"snapshot_id":self.snapshot,"platform":"windows","elements":nodes,"truncated":!queue.is_empty(),"note":"UI Automation control view; elevated applications and UAC secure desktop are outside ordinary-user access"}),
            )
        }
    }
    fn capture(&mut self, args: &Value) -> Result<Value> {
        let window = args["window_id"]
            .as_str()
            .map(str::parse::<usize>)
            .transpose()
            .context("window_id must be a native window ID from desktop_observe")?;
        let origin = if let Some(window) = window {
            let hwnd = HWND(window as *mut std::ffi::c_void);
            unsafe {
                if !IsWindow(hwnd).as_bool() {
                    bail!("Window no longer exists");
                }
                let mut rect = RECT::default();
                GetWindowRect(hwnd, &mut rect)?;
                (rect.left, rect.top)
            }
        } else {
            (0, 0)
        };
        match crate::wgc::capture(window) {
            Ok((bytes, width, height)) => {
                self.snapshot = uuid::Uuid::new_v4().to_string();
                self.refs.clear();
                self.screenshot = true;
                self.origin = origin;
                self.size = (width as i32, height as i32);
                let mut result = crate::save_png(bytes, width, height, &self.snapshot)?;
                result["capture_backend"] = json!("windows_graphics_capture");
                result["coordinate_space"] = json!("Screenshot-relative physical pixels");
                result["desktop_origin"] = json!({"x":origin.0,"y":origin.1});
                return Ok(result);
            }
            Err(error) => {
                if window.is_some() {
                    return Err(error.context("Requested window could not be captured by Windows.Graphics.Capture; no unrelated display is substituted"));
                }
                let mut result = self.capture_gdi()?;
                result["capture_backend"] = json!("gdi_visible_virtual_desktop");
                result["fallback_reason"] = json!(format!("{error:#}"));
                Ok(result)
            }
        }
    }
    fn capture_gdi(&mut self) -> Result<Value> {
        unsafe {
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            if width <= 0 || height <= 0 {
                bail!("No interactive desktop display is available");
            }
            let screen = GetDC(HWND::default());
            if screen.is_invalid() {
                bail!("Cannot access desktop display");
            }
            let memory = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            if memory.is_invalid() || bitmap.is_invalid() {
                ReleaseDC(HWND::default(), screen);
                bail!("Cannot allocate screenshot surface");
            }
            let original = SelectObject(memory, bitmap);
            let mut raw = vec![0u8; width as usize * height as usize * 4];
            let result = (|| -> Result<()> {
                BitBlt(
                    memory,
                    0,
                    0,
                    width,
                    height,
                    screen,
                    left,
                    top,
                    SRCCOPY | CAPTUREBLT,
                )?;
                let mut info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: width,
                        biHeight: -height,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                if GetDIBits(
                    memory,
                    bitmap,
                    0,
                    height as u32,
                    Some(raw.as_mut_ptr().cast()),
                    &mut info,
                    DIB_RGB_COLORS,
                ) == 0
                {
                    bail!("Desktop screenshot read failed");
                }
                Ok(())
            })();
            SelectObject(memory, original);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(memory);
            ReleaseDC(HWND::default(), screen);
            result?;
            for pixel in raw.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            let image = image::RgbaImage::from_raw(width as u32, height as u32, raw)
                .context("Invalid capture image dimensions")?;
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png)?;
            self.snapshot = uuid::Uuid::new_v4().to_string();
            self.refs.clear();
            self.screenshot = true;
            self.origin = (left, top);
            self.size = (width, height);
            let mut result = crate::save_png(
                bytes.into_inner(),
                width as u32,
                height as u32,
                &self.snapshot,
            )?;
            result["coordinate_space"] =
                json!("Screenshot pixels, relative to the virtual desktop image origin");
            result["desktop_origin"] = json!({"x":left,"y":top});
            Ok(result)
        }
    }
    fn act(&mut self, args: &Value) -> Result<Value> {
        unsafe {
            let action = args["action"].as_str().context("action is required")?;
            if action == "launch" {
                let app = args["app"]
                    .as_str()
                    .context("app executable path is required")?;
                let child=std::process::Command::new(app).spawn().context("Cannot launch application; app must name an executable, without shell arguments")?;
                self.snapshot.clear();
                self.refs.clear();
                return Ok(json!({"launch_requested":app,"pid":child.id()}));
            }
            if self.snapshot.is_empty() || args["snapshot_id"].as_str() != Some(&self.snapshot) {
                bail!("Stale snapshot_id; observe the desktop again");
            }
            match action {
                "click" => {
                    if let Some(reference) = args["ref"].as_str() {
                        let element = self.refs.get(reference).context("Stale element ref")?;
                        let pattern: IUIAutomationInvokePattern = element
                            .GetCurrentPattern(UIA_InvokePatternId)?
                            .cast()
                            .context("Element has no Invoke pattern; use screenshot coordinates")?;
                        pattern.Invoke()?;
                    } else {
                        if !self.screenshot {
                            bail!("Coordinates require a current screenshot");
                        }
                        let x = args["x"].as_i64().context("x is required")? as i32;
                        let y = args["y"].as_i64().context("y is required")? as i32;
                        if x < 0 || y < 0 || x >= self.size.0 || y >= self.size.1 {
                            bail!("Coordinates outside screenshot");
                        }
                        let movement = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT {
                                    dx: (((self.origin.0 + x - GetSystemMetrics(SM_XVIRTUALSCREEN))
                                        as i64
                                        * 65535)
                                        / (GetSystemMetrics(SM_CXVIRTUALSCREEN) - 1).max(1) as i64)
                                        as i32,
                                    dy: (((self.origin.1 + y - GetSystemMetrics(SM_YVIRTUALSCREEN))
                                        as i64
                                        * 65535)
                                        / (GetSystemMetrics(SM_CYVIRTUALSCREEN) - 1).max(1) as i64)
                                        as i32,
                                    dwFlags: MOUSEEVENTF_ABSOLUTE
                                        | MOUSEEVENTF_VIRTUALDESK
                                        | MOUSEEVENTF_MOVE,
                                    ..Default::default()
                                },
                            },
                        };
                        let down = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT {
                                    dwFlags: MOUSEEVENTF_LEFTDOWN,
                                    ..Default::default()
                                },
                            },
                        };
                        let up = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 {
                                mi: MOUSEINPUT {
                                    dwFlags: MOUSEEVENTF_LEFTUP,
                                    ..Default::default()
                                },
                            },
                        };
                        inject(&[movement, down, up])?;
                    }
                }
                "fill" => {
                    let reference = args["ref"].as_str().context("fill requires ref")?;
                    let element = self.refs.get(reference).context("Stale element ref")?;
                    let pattern: IUIAutomationValuePattern = element
                        .GetCurrentPattern(UIA_ValuePatternId)?
                        .cast()
                        .context("Element has no Value pattern")?;
                    let text = BSTR::from(args["text"].as_str().unwrap_or(""));
                    pattern.SetValue(&text)?;
                }
                "key" => {
                    let combination = args["key"].as_str().context("key is required")?;
                    let keys = keys(combination)?;
                    let mut inputs = Vec::new();
                    for key in &keys {
                        inputs.push(INPUT {
                            r#type: INPUT_KEYBOARD,
                            Anonymous: INPUT_0 {
                                ki: KEYBDINPUT {
                                    wVk: *key,
                                    ..Default::default()
                                },
                            },
                        });
                    }
                    for key in keys.iter().rev() {
                        inputs.push(INPUT {
                            r#type: INPUT_KEYBOARD,
                            Anonymous: INPUT_0 {
                                ki: KEYBDINPUT {
                                    wVk: *key,
                                    dwFlags: KEYEVENTF_KEYUP,
                                    ..Default::default()
                                },
                            },
                        });
                    }
                    inject(&inputs)?;
                }
                "scroll" => {
                    let delta = args["delta"].as_i64().unwrap_or(-500).clamp(-1200, 1200) as i32;
                    inject(&[INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                mouseData: delta as u32,
                                dwFlags: MOUSEEVENTF_WHEEL,
                                ..Default::default()
                            },
                        },
                    }])?;
                }
                _ => bail!("Unsupported desktop action"),
            }
            self.snapshot.clear();
            self.refs.clear();
            Ok(json!({"executed":true,"action":action,"requires_observation":true}))
        }
    }
}
unsafe fn inject(inputs: &[INPUT]) -> Result<()> {
    let count = SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    if count != inputs.len() as u32 {
        bail!("Input injection failed or was blocked by UIPI; elevated apps and secure desktop cannot be controlled from this ordinary-user helper");
    }
    Ok(())
}
fn keys(combination: &str) -> Result<Vec<VIRTUAL_KEY>> {
    combination
        .to_lowercase()
        .split('+')
        .map(|key| {
            Ok(match key {
                "ctrl" | "control" => VK_CONTROL,
                "shift" => VK_SHIFT,
                "alt" => VK_MENU,
                "meta" | "super" | "win" => VK_LWIN,
                "enter" | "return" => VK_RETURN,
                "escape" => VK_ESCAPE,
                "tab" => VK_TAB,
                "backspace" => VK_BACK,
                "delete" => VK_DELETE,
                "space" => VK_SPACE,
                "left" => VK_LEFT,
                "right" => VK_RIGHT,
                "up" => VK_UP,
                "down" => VK_DOWN,
                key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
                    VIRTUAL_KEY(key.to_ascii_uppercase().as_bytes()[0] as u16)
                }
                _ => bail!("Unsupported key combination"),
            })
        })
        .collect()
}
