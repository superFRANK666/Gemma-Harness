//! One genuine Windows.Graphics.Capture frame. The capture session is always
//! stopped, including on a bounded first-frame timeout.
use anyhow::{anyhow, Context, Result};
use std::{
    io::Cursor,
    sync::mpsc::{self, SyncSender},
    time::Duration,
};
use windows_capture::{
    capture::{Context as CaptureContext, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

type Pixels = (Vec<u8>, u32, u32);
struct OneFrame {
    sender: SyncSender<std::result::Result<Pixels, String>>,
}
impl GraphicsCaptureApiHandler for OneFrame {
    type Flags = SyncSender<std::result::Result<Pixels, String>>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    fn new(context: CaptureContext<Self::Flags>) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            sender: context.flags,
        })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        let result = (|| -> std::result::Result<Pixels, Self::Error> {
            let width = frame.width();
            let height = frame.height();
            let buffer = frame.buffer()?;
            let mut scratch = Vec::new();
            let bytes = buffer.as_nopadding_buffer(&mut scratch).to_vec();
            Ok((bytes, width, height))
        })();
        let _ = self
            .sender
            .try_send(result.map_err(|error| error.to_string()));
        control.stop();
        Ok(())
    }
    fn on_closed(&mut self) -> std::result::Result<(), Self::Error> {
        let _ = self.sender.try_send(Err(
            "Capture source closed before a frame was available".into()
        ));
        Ok(())
    }
}

fn one_frame<T: TryInto<GraphicsCaptureItemType> + Send + 'static>(
    source: T,
) -> Result<(Vec<u8>, u32, u32)> {
    let (tx, rx) = mpsc::sync_channel(1);
    let settings = Settings::new(
        source,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        tx,
    );
    let control = OneFrame::start_free_threaded(settings)
        .map_err(|error| anyhow!("Windows.Graphics.Capture startup failed: {error}"))?;
    let received = rx.recv_timeout(Duration::from_secs(10));
    let stopped = control
        .stop()
        .map_err(|error| anyhow!("Cannot stop WGC session: {error}"));
    let (pixels, width, height) = received
        .context("Windows.Graphics.Capture did not produce a frame within 10 seconds")?
        .map_err(anyhow::Error::msg)?;
    stopped?;
    let image = image::RgbaImage::from_raw(width, height, pixels)
        .context("WGC returned invalid RGBA dimensions")?;
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png)?;
    Ok((png.into_inner(), width, height))
}

pub fn capture(window: Option<usize>) -> Result<(Vec<u8>, u32, u32)> {
    if let Some(hwnd) = window {
        one_frame(Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void))
    } else {
        one_frame(Monitor::primary().context("No primary monitor is available")?)
    }
}
