use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{collections::HashMap, io::Cursor, time::Duration};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{ConnectionExt, ImageFormat},
        xtest::ConnectionExt as XTestExt,
    },
};
use zbus::{
    zvariant::{OwnedObjectPath, OwnedValue, Value as ZValue},
    Connection as Bus, Proxy,
};

type Accessible = (String, OwnedObjectPath);
pub struct Desktop {
    snapshot: String,
    refs: HashMap<String, Accessible>,
    screenshot: bool,
    portal: Option<Portal>,
}
struct Portal {
    bus: Bus,
    session: OwnedObjectPath,
    node: u32,
    fd: std::os::fd::OwnedFd,
}
impl Desktop {
    pub fn new() -> Self {
        Self {
            snapshot: String::new(),
            refs: HashMap::new(),
            screenshot: false,
            portal: None,
        }
    }
    pub async fn handle(&mut self, method: &str, args: &Value) -> Result<Value> {
        match method {
            "observe" => self.observe(args).await,
            "screenshot" => self.capture().await,
            "act" => self.act(args).await,
            "cancel" => {
                self.snapshot.clear();
                self.refs.clear();
                self.portal = None;
                Ok(json!({"cancelled":true}))
            }
            "capabilities" => self.capabilities().await,
            _ => bail!("Unknown native method"),
        }
    }
    fn wayland(&self) -> bool {
        std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland")
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
    async fn capabilities(&self) -> Result<Value> {
        let accessibility = tokio::time::timeout(Duration::from_secs(3), Self::a11y())
            .await
            .is_ok_and(|r| r.is_ok());
        let x11 = x11rb::connect(None).is_ok();
        let mut remote_desktop = false;
        let mut screen_cast = false;
        if self.wayland() {
            let portal_probe = async {
                let bus = Bus::session().await?;
                let remote = Proxy::new(
                    &bus,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.RemoteDesktop",
                )
                .await?;
                let types: u32 = remote.get_property("AvailableDeviceTypes").await?;
                let screen = Proxy::new(
                    &bus,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.ScreenCast",
                )
                .await?;
                let sources: u32 = screen.get_property("AvailableSourceTypes").await?;
                Ok::<_, anyhow::Error>((types & 3 == 3, sources & 1 != 0))
            };
            if let Ok(Ok((remote, screen))) =
                tokio::time::timeout(Duration::from_secs(3), portal_probe).await
            {
                remote_desktop = remote;
                screen_cast = screen;
            }
        }
        let gstreamer = tokio::process::Command::new("gst-launch-1.0")
            .arg("--version")
            .output()
            .await
            .is_ok_and(|o| o.status.success());
        Ok(
            json!({"platform":"linux","session":if self.wayland(){"wayland"}else{"x11"},"accessibility":accessibility,"x11_available":x11,"remote_desktop_portal":remote_desktop,"screen_cast_portal":screen_cast,"gstreamer":gstreamer,"coordinate_control":if self.wayland(){remote_desktop}else{x11},"screen_capture":if self.wayland(){remote_desktop&&screen_cast&&gstreamer}else{x11},"requires_user_authorization":self.wayland(),"note":"Wayland capture needs pipewiresrc, videoconvert and pngenc plugins in addition to gst-launch; no authorization is requested by this capability probe"}),
        )
    }
    async fn a11y() -> Result<Bus> {
        let session = Bus::session().await?;
        let proxy = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus").await?;
        let address: String = proxy
            .call("GetAddress", &())
            .await
            .context("AT-SPI accessibility bus is unavailable; enable desktop accessibility")?;
        Ok(zbus::connection::Builder::address(address.as_str())?
            .build()
            .await?)
    }
    async fn observe(&mut self, args: &Value) -> Result<Value> {
        let bus = Self::a11y().await?;
        let root = Proxy::new(
            &bus,
            "org.a11y.atspi.Registry",
            "/org/a11y/atspi/accessible/root",
            "org.a11y.atspi.Accessible",
        )
        .await?;
        let apps: Vec<Accessible> = root.call("GetChildren", &()).await?;
        self.snapshot = uuid::Uuid::new_v4().to_string();
        self.refs.clear();
        self.screenshot = false;
        let depth = args["depth"].as_u64().unwrap_or(4).clamp(1, 8);
        let mut pending: Vec<(Accessible, u64)> = apps.into_iter().rev().map(|a| (a, 0)).collect();
        let mut nodes = Vec::new();
        let mut count = 0;
        while let Some((address, level)) = pending.pop() {
            if nodes.len() >= 200 {
                break;
            }
            let proxy = match Proxy::new(
                &bus,
                address.0.as_str(),
                address.1.as_str(),
                "org.a11y.atspi.Accessible",
            )
            .await
            {
                Ok(p) => p,
                Err(_) => continue,
            };
            let name: String = proxy.get_property("Name").await.unwrap_or_default();
            if level == 0
                && args["app"]
                    .as_str()
                    .is_some_and(|filter| !name.to_lowercase().contains(&filter.to_lowercase()))
            {
                continue;
            }
            let role: String = proxy
                .call("GetRoleName", &())
                .await
                .unwrap_or_else(|_| "unknown".into());
            count += 1;
            let reference = format!("e{count}");
            self.refs.insert(reference.clone(), address.clone());
            nodes.push(json!({"ref":reference,"name":name.chars().take(180).collect::<String>(),"role":role,"depth":level}));
            if level < depth {
                if let Ok(children) = proxy
                    .call::<_, _, Vec<Accessible>>("GetChildren", &())
                    .await
                {
                    pending.extend(children.into_iter().rev().take(200).map(|a| (a, level + 1)));
                }
            }
        }
        Ok(
            json!({"snapshot_id":self.snapshot,"platform":"linux","session":if self.wayland(){"wayland"}else{"x11"},"elements":nodes,"truncated":!pending.is_empty(),"note":"AT-SPI semantic observation; custom canvas controls may require screenshots"}),
        )
    }
    async fn capture(&mut self) -> Result<Value> {
        self.snapshot = uuid::Uuid::new_v4().to_string();
        self.refs.clear();
        self.screenshot = true;
        if self.wayland() {
            if self.portal.is_none() {
                self.portal=Some(Portal::start().await.context("Wayland desktop control needs a RemoteDesktop portal (GNOME/KDE) and user authorization; screenshot-only wlr portals cannot grant input control")?);
            }
            let portal = self.portal.as_ref().unwrap();
            use std::os::fd::AsRawFd;
            let fd = portal.fd.as_raw_fd();
            // PipeWire remote must survive exec into the actual capture utility.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    bail!("Cannot pass PipeWire remote to capture process");
                }
            }
            let directory = std::path::PathBuf::from(
                std::env::var_os("HARNESS_ARTIFACT_DIR").context("Artifact directory missing")?,
            );
            let temp = directory.join(format!("pipewire-{}.png", uuid::Uuid::new_v4()));
            let result = tokio::process::Command::new("gst-launch-1.0")
                .args([
                    "-q",
                    "pipewiresrc",
                    &format!("fd={fd}"),
                    &format!("path={}", portal.node),
                    "num-buffers=1",
                    "!",
                    "videoconvert",
                    "!",
                    "pngenc",
                    "!",
                    "filesink",
                    &format!("location={}", temp.display()),
                ])
                .kill_on_drop(true)
                .output();
            let output=tokio::time::timeout(Duration::from_secs(15),result).await.context("PipeWire capture timed out")?.context("Install GStreamer gst-launch-1.0 with pipewiresrc, videoconvert and pngenc plugins")?;
            if !output.status.success() {
                bail!(
                    "PipeWire capture failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let bytes = std::fs::read(&temp)?;
            let image = image::load_from_memory(&bytes)?;
            let _ = std::fs::remove_file(temp);
            return crate::save_png(bytes, image.width(), image.height(), &self.snapshot);
        }
        let (connection, screen) = x11rb::connect(None).context("X11 DISPLAY is unavailable")?;
        let root = &connection.setup().roots[screen];
        let width = root.width_in_pixels;
        let height = root.height_in_pixels;
        let reply = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                root.root,
                0,
                0,
                width,
                height,
                u32::MAX,
            )?
            .reply()?;
        let format = connection
            .setup()
            .pixmap_formats
            .iter()
            .find(|f| f.depth == reply.depth)
            .context("Unknown X11 pixel format")?;
        if format.bits_per_pixel != 32 {
            bail!("X11 capture currently requires a 32-bit pixel format");
        }
        let image = image::RgbaImage::from_fn(width as u32, height as u32, |x, y| {
            let i = (y as usize * width as usize + x as usize) * 4;
            let b = &reply.data[i..i + 4];
            image::Rgba([b[2], b[1], b[0], 255])
        });
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png)?;
        crate::save_png(
            bytes.into_inner(),
            width as u32,
            height as u32,
            &self.snapshot,
        )
    }
    async fn act(&mut self, args: &Value) -> Result<Value> {
        let action = args["action"].as_str().context("action is required")?;
        if action == "launch" {
            let app = args["app"]
                .as_str()
                .context("app desktop file id is required")?;
            let output = tokio::process::Command::new("gtk-launch")
                .arg(app)
                .output()
                .await
                .context("gtk-launch is unavailable")?;
            if !output.status.success() {
                bail!(
                    "Application launch failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            self.snapshot.clear();
            self.refs.clear();
            self.screenshot = false;
            return Ok(json!({"launch_requested":app,"requires_observation":true}));
        }
        if self.snapshot.is_empty() || args["snapshot_id"].as_str() != Some(&self.snapshot) {
            bail!("Stale snapshot_id; observe again");
        }
        if let Some(reference) = args["ref"].as_str() {
            let address = self.refs.get(reference).context("Stale element ref")?;
            let bus = Self::a11y().await?;
            let success: bool = match action {
                "click" => Proxy::new(
                    &bus,
                    address.0.as_str(),
                    address.1.as_str(),
                    "org.a11y.atspi.Action",
                )
                .await?
                .call("DoAction", &(0i32,))
                .await
                .context("Element has no invoke action or became stale")?,
                "fill" => Proxy::new(
                    &bus,
                    address.0.as_str(),
                    address.1.as_str(),
                    "org.a11y.atspi.EditableText",
                )
                .await?
                .call("SetTextContents", &(args["text"].as_str().unwrap_or(""),))
                .await
                .context("Element is not editable or became stale")?,
                _ => bail!("Semantic actions support click and fill"),
            };
            if !success {
                bail!("Application rejected the accessibility action");
            }
        } else if self.wayland() {
            if self.portal.is_none() {
                self.portal = Some(Portal::start().await?);
            }
            self.portal
                .as_ref()
                .unwrap()
                .act(action, args, self.screenshot)
                .await?;
        } else {
            self.x11_act(action, args)?;
        }
        self.snapshot.clear();
        self.refs.clear();
        Ok(json!({"executed":true,"action":action,"requires_observation":true}))
    }
    fn x11_act(&self, action: &str, args: &Value) -> Result<()> {
        let (connection, screen) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen].root;
        match action {
            "click" => {
                if !self.screenshot {
                    bail!("Coordinates require a current screenshot");
                }
                let x = args["x"].as_i64().context("x is required")? as i16;
                let y = args["y"].as_i64().context("y is required")? as i16;
                connection
                    .xtest_fake_input(6, 0, 0, root, x, y, 0)?
                    .check()?;
                connection
                    .xtest_fake_input(4, 1, 0, root, 0, 0, 0)?
                    .check()?;
                connection
                    .xtest_fake_input(5, 1, 0, root, 0, 0, 0)?
                    .check()?;
            }
            "scroll" => {
                let delta = args["delta"].as_i64().unwrap_or(-500);
                let button = if delta < 0 { 5 } else { 4 };
                for _ in 0..(delta.unsigned_abs() / 100).clamp(1, 20) {
                    connection
                        .xtest_fake_input(4, button, 0, root, 0, 0, 0)?
                        .check()?;
                    connection
                        .xtest_fake_input(5, button, 0, root, 0, 0, 0)?
                        .check()?;
                }
            }
            "key" => {
                let symbols = keysyms(args["key"].as_str().context("key is required")?)?;
                let first = connection.setup().min_keycode;
                let last = connection.setup().max_keycode;
                let mapping = connection
                    .get_keyboard_mapping(first, last - first + 1)?
                    .reply()?;
                let mut codes = Vec::new();
                for symbol in symbols {
                    let index = mapping
                        .keysyms
                        .chunks(mapping.keysyms_per_keycode as usize)
                        .position(|chunk| chunk.contains(&symbol))
                        .context("Requested key is unavailable in keyboard layout")?;
                    codes.push(first + index as u8);
                }
                for code in &codes {
                    connection
                        .xtest_fake_input(2, *code, 0, root, 0, 0, 0)?
                        .check()?;
                }
                for code in codes.iter().rev() {
                    connection
                        .xtest_fake_input(3, *code, 0, root, 0, 0, 0)?
                        .check()?;
                }
            }
            _ => bail!("Unsupported coordinate action"),
        }
        connection.flush()?;
        Ok(())
    }
}
fn keysyms(combination: &str) -> Result<Vec<u32>> {
    combination
        .to_lowercase()
        .split('+')
        .map(|s| {
            Ok(match s {
                "ctrl" | "control" => 0xffe3,
                "shift" => 0xffe1,
                "alt" => 0xffe9,
                "meta" | "super" => 0xffeb,
                "enter" | "return" => 0xff0d,
                "tab" => 0xff09,
                "escape" => 0xff1b,
                "backspace" => 0xff08,
                "delete" => 0xffff,
                "left" => 0xff51,
                "up" => 0xff52,
                "right" => 0xff53,
                "down" => 0xff54,
                "space" => 32,
                s if s.chars().count() == 1 => s.chars().next().unwrap() as u32,
                _ => bail!("Unsupported key: {s}"),
            })
        })
        .collect()
}

impl Portal {
    async fn start() -> Result<Self> {
        let bus = Bus::session().await?;
        let mut create = HashMap::new();
        let session_token = format!("harness{}", uuid::Uuid::new_v4().simple());
        create.insert("session_handle_token", ZValue::from(session_token.as_str()));
        let response = request(
            &bus,
            "org.freedesktop.portal.RemoteDesktop",
            "CreateSession",
            &create,
            None,
        )
        .await?;
        let session: OwnedObjectPath = response
            .get("session_handle")
            .context("Portal returned no session")?
            .try_clone()?
            .try_into()?;
        let mut devices = HashMap::new();
        devices.insert("types", ZValue::from(3u32));
        request(
            &bus,
            "org.freedesktop.portal.RemoteDesktop",
            "SelectDevices",
            &devices,
            Some(&session),
        )
        .await?;
        let mut sources = HashMap::new();
        sources.insert("types", ZValue::from(1u32));
        sources.insert("multiple", ZValue::from(false));
        request(
            &bus,
            "org.freedesktop.portal.ScreenCast",
            "SelectSources",
            &sources,
            Some(&session),
        )
        .await?;
        let token = format!("harness{}", uuid::Uuid::new_v4().simple());
        let request_path = path_for_request(&bus, &token)?;
        let request_proxy = Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            request_path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .await?;
        let mut responses = request_proxy.receive_signal("Response").await?;
        let remote = Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.RemoteDesktop",
        )
        .await?;
        let options = HashMap::from([("handle_token", ZValue::from(token.as_str()))]);
        let _: OwnedObjectPath = remote.call("Start", &(&session, "", options)).await?;
        let message = tokio::time::timeout(Duration::from_secs(60), responses.next())
            .await?
            .context("Portal request closed")?;
        let (code, values): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
        if code != 0 {
            bail!("Desktop sharing authorization was cancelled or denied (portal code {code})");
        }
        let streams: Vec<(u32, HashMap<String, OwnedValue>)> = values
            .get("streams")
            .context("Portal returned no screen streams")?
            .try_clone()?
            .try_into()?;
        let node = streams.first().context("No screen selected")?.0;
        let fd: zbus::zvariant::OwnedFd = Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.ScreenCast",
        )
        .await?
        .call(
            "OpenPipeWireRemote",
            &(&session, HashMap::<&str, ZValue>::new()),
        )
        .await?;
        Ok(Self {
            bus,
            session,
            node,
            fd: fd.into(),
        })
    }
    async fn act(&self, action: &str, args: &Value, screenshot: bool) -> Result<()> {
        let proxy = Proxy::new(
            &self.bus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.RemoteDesktop",
        )
        .await?;
        let options = HashMap::<&str, ZValue>::new();
        match action {
            "click" => {
                if !screenshot {
                    bail!("Coordinates require current screenshot");
                }
                let x = args["x"].as_f64().context("x required")?;
                let y = args["y"].as_f64().context("y required")?;
                proxy
                    .call::<_, _, ()>(
                        "NotifyPointerMotionAbsolute",
                        &(&self.session, &options, self.node, x, y),
                    )
                    .await?;
                proxy
                    .call::<_, _, ()>(
                        "NotifyPointerButton",
                        &(&self.session, &options, 272i32, 1u32),
                    )
                    .await?;
                proxy
                    .call::<_, _, ()>(
                        "NotifyPointerButton",
                        &(&self.session, &options, 272i32, 0u32),
                    )
                    .await?;
            }
            "scroll" => {
                proxy
                    .call::<_, _, ()>(
                        "NotifyPointerAxis",
                        &(
                            &self.session,
                            &options,
                            0.0f64,
                            -args["delta"].as_f64().unwrap_or(-500.0),
                        ),
                    )
                    .await?;
            }
            "key" => {
                let symbols = keysyms(args["key"].as_str().context("key required")?)?;
                for symbol in &symbols {
                    proxy
                        .call::<_, _, ()>(
                            "NotifyKeyboardKeysym",
                            &(&self.session, &options, *symbol as i32, 1u32),
                        )
                        .await?;
                }
                for symbol in symbols.iter().rev() {
                    proxy
                        .call::<_, _, ()>(
                            "NotifyKeyboardKeysym",
                            &(&self.session, &options, *symbol as i32, 0u32),
                        )
                        .await?;
                }
            }
            _ => bail!("Unsupported Wayland action; use semantic fill for text"),
        }
        Ok(())
    }
}
fn path_for_request(bus: &Bus, token: &str) -> Result<String> {
    let sender = bus
        .unique_name()
        .context("Session bus has no unique name")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    Ok(format!(
        "/org/freedesktop/portal/desktop/request/{sender}/{token}"
    ))
}
async fn request(
    bus: &Bus,
    interface: &str,
    method: &str,
    options: &HashMap<&str, ZValue<'_>>,
    session: Option<&OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>> {
    let token = format!("harness{}", uuid::Uuid::new_v4().simple());
    let path = path_for_request(bus, &token)?;
    let request = Proxy::new(
        bus,
        "org.freedesktop.portal.Desktop",
        path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await?;
    let mut responses = request.receive_signal("Response").await?;
    let portal = Proxy::new(
        bus,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        interface,
    )
    .await?;
    let mut args: HashMap<&str, ZValue> = options
        .iter()
        .map(|(k, v)| Ok((*k, v.try_clone()?)))
        .collect::<Result<_>>()?;
    args.insert("handle_token", ZValue::from(token.as_str()));
    let _: OwnedObjectPath = if let Some(session) = session {
        portal.call(method, &(session, args)).await?
    } else {
        portal.call(method, &(args,)).await?
    };
    let message = tokio::time::timeout(Duration::from_secs(30), responses.next())
        .await?
        .context("Portal request closed")?;
    let (code, response): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
    if code != 0 {
        bail!("Portal request {method} denied/cancelled ({code})");
    }
    Ok(response)
}
