//! Video capture: game capture on a window, window capture as fallback,
//! monitor capture.

use std::time::{Duration, Instant};

/// How long game capture may try to hook before window capture takes over.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// OBS's window id, `title:class:exe`, each part escaped the way OBS (and
/// libobs-window-helper, which keeps its encoder private) does it.
pub fn window_id(title: &str, class: &str, exe: &str) -> String {
    let encode = |part: &str| part.replace('#', "#22").replace(':', "#3A");
    [title, class, exe].map(encode).join(":")
}

/// Decides when game capture has had long enough to hook the game.
pub struct HookWatch {
    started: Instant,
    /// A capture is running: the game was hooked, or the fallback took over.
    hooked: bool,
}

impl HookWatch {
    pub fn new(now: Instant) -> Self {
        HookWatch {
            started: now,
            hooked: false,
        }
    }

    pub fn hooked(&mut self) {
        self.hooked = true;
    }

    /// True once, [`HOOK_TIMEOUT`] after `new` without a hook.
    pub fn should_fall_back(&mut self, now: Instant) -> bool {
        if self.hooked || now.saturating_duration_since(self.started) < HOOK_TIMEOUT {
            return false;
        }
        self.hooked = true;
        true
    }
}

#[cfg(windows)]
pub use obs::Capture;

#[cfg(windows)]
mod obs {
    use std::time::Instant;

    use libobs_simple::sources::windows::monitor_capture::MonitorCaptureSource;
    use libobs_simple::sources::windows::{
        GameCaptureSource, GameCaptureSourceBuilder, HookedSignal, MonitorCaptureSourceBuilder,
        ObsDisplayCaptureMethod, ObsGameCaptureMode, ObsHookableSourceTrait,
        ObsWindowCaptureMethod, WindowCaptureSource, WindowCaptureSourceBuilder,
    };
    use libobs_wrapper::context::ObsContext;
    use libobs_wrapper::data::object::ObsObjectTrait;
    use libobs_wrapper::enums::ObsOrderMovement;
    use libobs_wrapper::scenes::{ObsSceneItemRef, ObsSceneRef, SceneItemTrait};
    use libobs_wrapper::signals::SignalReceiver;
    use libobs_wrapper::sources::ObsSourceBuilder;
    use libobs_wrapper::sys as obs_sys;
    use libobs_wrapper::utils::ObsError;
    use obs_protocol::{CaptureKind, Reply, Source};

    use super::{window_id, HookWatch};

    /// The video source in the scene.
    pub enum Capture {
        /// Game capture, waiting for its hook or hooked.
        Game {
            item: ObsSceneItemRef<GameCaptureSource>,
            hooked: SignalReceiver<HookedSignal>,
            watch: HookWatch,
            window: String,
        },
        /// Window capture (WGC) after game capture did not hook.
        Window {
            _item: ObsSceneItemRef<WindowCaptureSource>,
        },
        Monitor {
            _item: ObsSceneItemRef<MonitorCaptureSource>,
        },
    }

    impl Capture {
        /// Adds the capture for `source` to the scene, fitted to the canvas
        /// with black bars. Monitor capture runs straight away, so it comes
        /// with its `Hooked` reply.
        pub fn start(
            ctx: &ObsContext,
            scene: &mut ObsSceneRef,
            source: &Source,
            now: Instant,
        ) -> Result<(Capture, Option<Reply>), ObsError> {
            match source {
                Source::Window { title, class, exe } => {
                    let window = window_id(title, class, exe);
                    let source = ctx
                        .source_builder::<GameCaptureSourceBuilder, _>("game capture")?
                        .set_capture_mode(ObsGameCaptureMode::CaptureSpecificWindow)
                        .set_window_raw(window.as_str())
                        .set_capture_cursor(true)
                        .set_anti_cheat_hook(true)
                        .build()?;
                    // Subscribed before the source is shown, so no hook is missed.
                    let hooked = source.source_specific_signals().on_hooked()?;
                    let item = scene.add(source)?;
                    item.fit_source_to_screen()?;
                    let capture = Capture::Game {
                        item,
                        hooked,
                        watch: HookWatch::new(now),
                        window,
                    };
                    Ok((capture, None))
                }
                Source::Monitor { index } => {
                    dpi_aware();
                    // Automatic, as the Old engine chooses: DXGI when the
                    // monitor is on the GPU OBS renders with, otherwise
                    // Windows Graphics Capture, which crosses adapters
                    // (forcing DXGI gives black clips on laptops with two
                    // GPUs).
                    let item = ctx
                        .source_builder::<MonitorCaptureSourceBuilder, _>("monitor capture")?
                        .set_monitor_id_raw(monitor_id(*index))
                        .set_capture_cursor(true)
                        .set_capture_method(ObsDisplayCaptureMethod::MethodAuto)
                        .add_to_scene(scene)?;
                    item.fit_source_to_screen()?;
                    let running = Reply::Hooked {
                        capture: CaptureKind::Monitor,
                    };
                    Ok((Capture::Monitor { _item: item }, Some(running)))
                }
            }
        }

        /// Reports the game hook; replaces game capture by window capture
        /// when the hook does not come in time.
        pub fn tick(
            &mut self,
            ctx: &ObsContext,
            scene: &mut ObsSceneRef,
            now: Instant,
        ) -> Result<Option<Reply>, ObsError> {
            let Capture::Game {
                item,
                hooked,
                watch,
                window,
            } = self
            else {
                return Ok(None);
            };
            let signalled = hooked.try_recv().is_ok();
            while hooked.try_recv().is_ok() {}
            // The signal only arrives if its payload parses; a picture is
            // proof of a hook as well.
            if signalled || (!watch.hooked && has_picture(item.inner_source())?) {
                watch.hooked();
                return Ok(Some(Reply::Hooked {
                    capture: CaptureKind::Game,
                }));
            }
            if !watch.should_fall_back(now) {
                return Ok(None);
            }
            log::warn!("game capture did not hook {window:?}; using window capture");
            let window = window.clone();
            scene.remove_item(&*item)?;
            let item = ctx
                .source_builder::<WindowCaptureSourceBuilder, _>("window capture")?
                .set_window_raw(window.as_str())
                .set_cursor(true)
                .set_capture_method(ObsWindowCaptureMethod::MethodWgc)
                .add_to_scene(scene)?;
            item.fit_source_to_screen()?;
            // Below the watermark, where the game capture was.
            item.move_order(ObsOrderMovement::Bottom)?;
            *self = Capture::Window { _item: item };
            Ok(Some(Reply::Hooked {
                capture: CaptureKind::Window,
            }))
        }
    }

    /// DXGI capture (which the automatic method may pick) needs a process
    /// that is aware of per-monitor DPI. libobs-simple only sets that when
    /// DXGI is asked for by name, so this does what it did then. Fails, which
    /// is fine, once the awareness is set.
    fn dpi_aware() {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        // SAFETY: only changes this process's DPI awareness.
        if let Err(e) =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        {
            log::debug!("DPI awareness not changed: {e}");
        }
    }

    /// Whether game capture delivers a picture: its size is 0 until hooked.
    fn has_picture(source: &GameCaptureSource) -> Result<bool, ObsError> {
        let handle = source.__native_handle();
        source.runtime().run_with_obs_result(move || {
            // SAFETY: runs on the OBS thread; `handle` keeps the source alive.
            unsafe { obs_sys::obs_source_get_width(handle.raw_ptr_unchecked()) > 0 }
        })
    }

    /// OBS's id for monitor `index` in the app's order (`monitors`, primary
    /// first): the device interface name, which is what OBS's display
    /// capture lists. Falls back to the GDI device name, then the primary.
    fn monitor_id(index: usize) -> String {
        let monitors = crate::monitors::monitor_names();
        let Some(name) = monitors.get(index).or(monitors.first()) else {
            return String::new();
        };
        device_interface_name(name).unwrap_or_else(|| name.clone())
    }

    fn device_interface_name(device: &str) -> Option<String> {
        use windows::core::HSTRING;
        use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW};
        use windows::Win32::UI::WindowsAndMessaging::EDD_GET_DEVICE_INTERFACE_NAME;

        let mut info = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a DISPLAY_DEVICEW with its size filled in.
        let found = unsafe {
            EnumDisplayDevicesW(
                &HSTRING::from(device),
                0,
                &mut info,
                EDD_GET_DEVICE_INTERFACE_NAME,
            )
        };
        let len = info
            .DeviceID
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.DeviceID.len());
        let id = String::from_utf16_lossy(&info.DeviceID[..len]);
        (found.as_bool() && !id.is_empty()).then_some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fallback_before_five_seconds() {
        let start = Instant::now();
        let mut watch = HookWatch::new(start);
        assert!(!watch.should_fall_back(start));
        assert!(!watch.should_fall_back(start + Duration::from_millis(4_900)));
    }

    #[test]
    fn fallback_after_five_seconds_without_hook() {
        let start = Instant::now();
        let mut watch = HookWatch::new(start);
        assert!(watch.should_fall_back(start + Duration::from_secs(5)));
    }

    #[test]
    fn no_fallback_once_hooked() {
        let start = Instant::now();
        let mut watch = HookWatch::new(start);
        watch.hooked();
        assert!(!watch.should_fall_back(start + Duration::from_secs(60)));
    }

    #[test]
    fn window_id_is_title_class_exe() {
        assert_eq!(
            window_id(
                "Rocket League",
                "LaunchUnrealUWindowsClient",
                "RocketLeague.exe"
            ),
            "Rocket League:LaunchUnrealUWindowsClient:RocketLeague.exe"
        );
        assert_eq!(window_id("", "", "Discord.exe"), "::Discord.exe");
    }

    #[test]
    fn window_id_escapes_hash_and_colon() {
        assert_eq!(window_id("a#b:c", "#:", "x.exe"), "a#22b#3Ac:#22#3A:x.exe");
    }

    #[test]
    fn fallback_only_once() {
        let start = Instant::now();
        let mut watch = HookWatch::new(start);
        assert!(watch.should_fall_back(start + Duration::from_secs(6)));
        assert!(!watch.should_fall_back(start + Duration::from_secs(7)));
        assert!(!watch.should_fall_back(start + Duration::from_secs(60)));
    }
}
