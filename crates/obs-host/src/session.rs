//! One OBS recording session: scene with the capture and the watermark,
//! audio sources with meters, the encoders, the replay buffer and the
//! start/stop recording output.

use std::path::{Path, PathBuf};

use obs_protocol::{AudioInput, AudioLevel, Reply};

/// OBS has six audio tracks (mixes).
const TRACKS: usize = 6;

/// A path as libobs takes it: UTF-8, without Windows' `\\?\` prefix (which
/// `canonicalize` and some launchers add, and which OBS does not handle).
pub fn obs_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.into_owned()
    }
}

/// The box the watermark is fitted into, in canvas coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WatermarkBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Where the watermark goes. Same rule as the old engine
/// (`encode::watermark::watermark_for`), worked out in output pixels: 4.5 %
/// of the frame height (at least 18 px), 2 % margin from the bottom-right
/// corner, narrower frames shrink it to fit. OBS fits the image into this
/// box (scale inner, aligned bottom right), which gives the same size; the
/// scene uses canvas coordinates, so the box is scaled from output to canvas.
pub fn watermark_box(canvas: (u32, u32), output: (u32, u32)) -> WatermarkBox {
    let (out_w, out_h) = (output.0.max(1), output.1.max(1));
    let margin = (f64::from(out_h) * 0.02).round() as u32;
    let height = ((f64::from(out_h) * 0.045).round() as u32)
        .max(18)
        .min(out_h);
    let width = out_w.saturating_sub(2 * margin).max(1);
    let sx = canvas.0 as f32 / out_w as f32;
    let sy = canvas.1 as f32 / out_h as f32;
    WatermarkBox {
        x: out_w.saturating_sub(margin + width) as f32 * sx,
        y: out_h.saturating_sub(margin + height) as f32 * sy,
        width: width as f32 * sx,
        height: height as f32 * sy,
    }
}

/// An OBS meter value (dBFS) as a linear level, 0..=1. Never NaN.
pub fn level_from_db(db: f32) -> f32 {
    let level = 10f32.powf(db / 20.0);
    if level.is_nan() {
        0.0
    } else {
        level.clamp(0.0, 1.0)
    }
}

/// The loudest of a set of meter values, as a level.
pub fn peak_level(dbs: impl IntoIterator<Item = f32>) -> f32 {
    dbs.into_iter().map(level_from_db).fold(0.0, f32::max)
}

/// The replies for a recording output that stopped by itself (an encoder
/// error, a full disk) while writing `path`: what OBS wrote, if anything,
/// goes to the app as a finished recording, then the error.
pub fn recording_stopped_by_itself(path: PathBuf) -> Vec<Reply> {
    let error = Reply::Error {
        message: "the recording stopped by itself".into(),
    };
    let written = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 0);
    if written {
        vec![Reply::RecordingSaved { path }, error]
    } else {
        vec![error]
    }
}

/// What one audio source delivered, for `Reply::Levels`.
///
/// OBS does not say whether a source found what it captures:
/// `obs_source_audio_active` is a flag sources set for themselves, and
/// nothing says the WASAPI sources clear it when they find no device or,
/// per app, no window of the app. The volume meter, though, only updates
/// when the source outputs audio, so a source counts as active from its
/// first meter update on (and stays so until the helper restarts).
#[derive(Debug, Default)]
pub struct AudioActivity {
    /// The source has delivered audio (silence included) at least once.
    heard: bool,
}

impl AudioActivity {
    /// The level from the meter updates since the last call, each given as
    /// its per-channel peaks in dBFS.
    pub fn take<P: IntoIterator<Item = f32>>(
        &mut self,
        updates: impl IntoIterator<Item = P>,
    ) -> AudioLevel {
        let mut level = 0.0f32;
        for peaks in updates {
            self.heard = true;
            level = level.max(peak_level(peaks));
        }
        AudioLevel {
            level,
            active: self.heard,
        }
    }
}

/// The tracks (0-based OBS mixes) that get an audio encoder: track 1, the
/// mix, always, plus every track a source is routed to.
pub fn used_tracks(audio: &[AudioInput]) -> Vec<usize> {
    let mask = audio.iter().fold(1, |mask, input| mask | input.mixers);
    (0..TRACKS).filter(|t| mask & (1 << t) != 0).collect()
}

#[cfg(windows)]
pub use obs::Session;

#[cfg(windows)]
mod obs {
    use std::ffi::{CStr, CString};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use libobs_wrapper::context::ObsContext;
    use libobs_wrapper::data::audio::{
        AudioEncoderSlot, AudioMixIndex, AudioMixMask, LinearVolume, ObsAudioInfo,
    };
    use libobs_wrapper::data::object::ObsObjectTrait;
    use libobs_wrapper::data::output::{ObsOutputRef, ObsOutputTrait, ObsReplayBufferOutputRef};
    use libobs_wrapper::data::video::ObsVideoInfoBuilder;
    use libobs_wrapper::data::ObsDataSetters;
    use libobs_wrapper::encoders::audio::ObsAudioEncoder;
    use libobs_wrapper::encoders::video::ObsVideoEncoder;
    use libobs_wrapper::enums::{
        ObsAlignment, ObsBoundsType, ObsSamplesPerSecond, ObsScaleType, ObsSpeakerLayout,
    };
    use libobs_wrapper::graphics::Vec2;
    use libobs_wrapper::scenes::{
        ObsSceneItemRef, ObsSceneRef, ObsTransformInfoBuilder, SceneItemTrait,
    };
    use libobs_wrapper::signals::SignalReceiver;
    use libobs_wrapper::sources::{
        ObsFaderScale, ObsPeakMeterType, ObsSourceAudioExt, ObsSourceRef, ObsVolumeMeter,
        VolumeMeterFrame,
    };
    use libobs_wrapper::sys as obs_sys;
    use libobs_wrapper::utils::{
        AudioEncoderInfo, ObsError, ObsPath, OutputInfo, StartupInfo, StartupPaths,
        VideoEncoderInfo,
    };
    use obs_protocol::{AudioInput, AudioKind, AudioLevel, Encoder, HostConfig, Reply};

    use super::{
        obs_path, recording_stopped_by_itself, used_tracks, watermark_box, AudioActivity, TRACKS,
    };
    use crate::capture::{window_id, Capture};
    use crate::host::{logging, Event};

    const AUDIO_KBPS: i64 = 160;
    const REPLAY_MAX_MB: i64 = 4096;
    /// How long stopping an output may take before we give up waiting.
    const STOP_TIMEOUT: Duration = Duration::from_secs(15);

    pub struct Session {
        capture: Capture,
        _watermark: Option<ObsSceneItemRef<ObsSourceRef>>,
        /// One per `HostConfig.audio` entry, in that order.
        meters: Vec<Meter>,
        replay: ObsReplayBufferOutputRef,
        /// The folder the replay buffer saves into now.
        replay_dir: PathBuf,
        /// The replay buffer stopped by itself and could not be restarted.
        replay_failed: bool,
        /// Shares the replay buffer's encoders.
        recording: ObsOutputRef,
        recording_path: Option<PathBuf>,
        /// The video encoder in use (after a fallback: x264).
        encoder: Encoder,
        config: HostConfig,
        /// Saves and recording stops, which wait for OBS off the main loop.
        jobs: Vec<JoinHandle<()>>,
        events: Sender<Event>,
        scene: ObsSceneRef,
        ctx: ObsContext,
    }

    impl Session {
        /// Starts OBS for `config` and the replay buffer. The replies are for
        /// after `Ready` (encoder fallback, monitor capture running).
        pub fn start(
            config: HostConfig,
            events: Sender<Event>,
        ) -> Result<(Session, Vec<Reply>), String> {
            let mut ctx = start_obs(&config)?;
            logging::obs_started();
            let mut scene = ctx.scene("Replayr", Some(0)).map_err(context("scene"))?;
            let (capture, running) =
                Capture::start(&ctx, &mut scene, &config.source, Instant::now())
                    .map_err(context("capture"))?;
            let watermark = config.watermark.as_deref().and_then(|file| {
                add_watermark(&ctx, &mut scene, file, &config)
                    .map_err(|e| log::warn!("watermark {}: {e}", file.display()))
                    .ok()
            });
            let meters = config
                .audio
                .iter()
                .enumerate()
                .map(|(index, input)| Meter::new(&ctx, &mut scene, index, input))
                .collect();
            let replay_dir = std::env::temp_dir();
            let (replay, recording) =
                create_outputs(&mut ctx, &config, &replay_dir).map_err(context("outputs"))?;

            let mut session = Session {
                capture,
                _watermark: watermark,
                meters,
                replay,
                replay_dir,
                replay_failed: false,
                recording,
                recording_path: None,
                encoder: config.encoder,
                config,
                jobs: Vec::new(),
                events,
                scene,
                ctx,
            };
            let fallback = session.start_replay_or_fall_back()?;
            Ok((session, fallback.into_iter().chain(running).collect()))
        }

        /// Levels, the game hook, and outputs that stopped by themselves.
        pub fn tick(&mut self, now: Instant) -> Vec<Reply> {
            self.jobs.retain(|job| !job.is_finished());
            let mut replies = vec![Reply::Levels(
                self.meters.iter_mut().map(Meter::take_level).collect(),
            )];
            match self.capture.tick(&self.ctx, &mut self.scene, now) {
                Ok(reply) => replies.extend(reply),
                Err(e) => replies.push(error(format!("window capture: {e}"))),
            }
            replies.extend(self.check_outputs());
            replies
        }

        /// Saves the replay buffer into `dir`; `Saved` comes from a job.
        pub fn save(&mut self, dir: PathBuf) -> Option<Reply> {
            if let Err(message) = self.use_replay_dir(&dir) {
                return Some(Reply::Error { message });
            }
            let replay = self.replay.clone();
            self.spawn_job(move || match replay.save_buffer() {
                Ok(path) => Reply::Saved {
                    path: native_path(&path),
                },
                Err(e) => error(format!("saving the replay failed: {e}")),
            });
            None
        }

        /// Starts recording into a new file in `dir`.
        pub fn record_start(&mut self, dir: PathBuf) -> Option<Reply> {
            if self.recording_path.is_some() {
                return Some(error("already recording".into()));
            }
            let name = format!(
                "Recording {}.mp4",
                chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
            );
            let path = dir.join(name);
            match self.start_recording(&dir, &path) {
                Ok(()) => {
                    self.recording_path = Some(path);
                    None
                }
                Err(e) => Some(error(e)),
            }
        }

        /// Stops the recording; `RecordingSaved` comes from a job.
        pub fn record_stop(&mut self) -> Option<Reply> {
            let Some(path) = self.recording_path.take() else {
                return Some(error("not recording".into()));
            };
            let recording = self.recording.clone();
            self.spawn_job(move || match recording.stop_with_timeout(STOP_TIMEOUT) {
                Ok(()) => Reply::RecordingSaved { path },
                Err(e) => error(format!("stopping the recording failed: {e}")),
            });
            None
        }

        /// Finishes running jobs, stops the outputs and drops everything, after
        /// which libobs shuts down.
        pub fn shutdown(mut self) -> Vec<Reply> {
            for job in self.jobs.drain(..) {
                let _ = job.join();
            }
            let mut replies = Vec::new();
            if let Some(path) = self.recording_path.take() {
                replies.push(match self.recording.stop_with_timeout(STOP_TIMEOUT) {
                    Ok(()) => Reply::RecordingSaved { path },
                    Err(e) => error(format!("stopping the recording failed: {e}")),
                });
            }
            if let Err(e) = self.replay.stop_with_timeout(STOP_TIMEOUT) {
                log::warn!("stopping the replay buffer: {e}");
            }
            replies
        }

        fn spawn_job(&mut self, job: impl FnOnce() -> Reply + Send + 'static) {
            let events = self.events.clone();
            self.jobs.push(std::thread::spawn(move || {
                let _ = events.send(Event::Reply(job()));
            }));
        }

        /// Puts a new `encoder` on both outputs and starts the replay buffer.
        fn start_replay(&self, encoder: Encoder) -> Result<(), ObsError> {
            let video = video_encoder(&self.ctx, encoder, &self.config)?;
            self.replay.set_video_encoder(video.clone())?;
            self.recording.set_video_encoder(video)?;
            self.replay.start()
        }

        fn start_replay_or_fall_back(&mut self) -> Result<Option<Reply>, String> {
            let Err(e) = self.start_replay(self.encoder) else {
                return Ok(None);
            };
            if self.encoder == Encoder::X264 {
                return Err(format!("the replay buffer did not start: {e}"));
            }
            self.fall_back(e.to_string()).map(Some)
        }

        fn fall_back(&mut self, error: String) -> Result<Reply, String> {
            log::warn!("{}: {error}; switching to x264", self.encoder.obs_id());
            self.start_replay(Encoder::X264)
                .map_err(|e| format!("the replay buffer did not start with x264 either: {e}"))?;
            let from = std::mem::replace(&mut self.encoder, Encoder::X264);
            Ok(Reply::EncoderFallback { from, error })
        }

        /// Outputs stop by themselves on an encoder error or a full disk.
        fn check_outputs(&mut self) -> Vec<Reply> {
            let mut replies = Vec::new();
            if self.recording_path.is_some() && !self.recording.is_active().unwrap_or(true) {
                if let Some(path) = self.recording_path.take() {
                    replies.extend(recording_stopped_by_itself(path));
                }
            }
            if !self.replay_failed && !self.replay.is_active().unwrap_or(true) {
                let restarted = if self.encoder == Encoder::X264 {
                    Err("the replay buffer stopped by itself".to_string())
                } else {
                    self.fall_back("the encoder stopped".into())
                };
                match restarted {
                    Ok(reply) => replies.push(reply),
                    Err(message) => {
                        self.replay_failed = true;
                        replies.push(Reply::Error { message });
                    }
                }
            }
            replies
        }

        fn use_replay_dir(&mut self, dir: &Path) -> Result<(), String> {
            create_dir(dir)?;
            if dir != self.replay_dir {
                set_output_string(&self.replay, c"directory", &obs_path(dir))?;
                self.replay_dir = dir.to_path_buf();
            }
            Ok(())
        }

        fn start_recording(&self, dir: &Path, path: &Path) -> Result<(), String> {
            create_dir(dir)?;
            let start = || -> Result<(), ObsError> {
                let mut settings = self.ctx.data()?;
                settings.set_string("path", obs_path(path))?;
                self.recording.update_settings(settings)?;
                self.recording.start()
            };
            start().map_err(|e| format!("the recording did not start: {e}"))
        }
    }

    /// The meter of one audio input.
    struct Meter {
        /// None if its source could not be made.
        parts: Option<(
            ObsSourceRef,
            ObsVolumeMeter,
            SignalReceiver<VolumeMeterFrame>,
        )>,
        activity: AudioActivity,
    }

    impl Meter {
        /// Adds the source for `input` to the scene (that makes it active).
        fn new(
            ctx: &ObsContext,
            scene: &mut ObsSceneRef,
            index: usize,
            input: &AudioInput,
        ) -> Meter {
            let parts = audio_source(ctx, index, input).and_then(|source| {
                scene.add(source.clone())?;
                let meter = source.create_volume_meter(
                    ObsFaderScale::Logarithmic,
                    ObsPeakMeterType::SamplePeak,
                )?;
                let updates = meter.on_update();
                Ok((source, meter, updates))
            });
            let parts = match parts {
                Ok(parts) => Some(parts),
                Err(e) => {
                    log::warn!("audio source {} ({}): {e}", index + 1, input.id);
                    None
                }
            };
            Meter {
                parts,
                activity: AudioActivity::default(),
            }
        }

        /// The peak since the last call (after volume), 0..=1, and whether
        /// the source delivers; a source OBS could not make never does.
        fn take_level(&mut self) -> AudioLevel {
            let Some((_, meter, updates)) = &self.parts else {
                return AudioLevel {
                    level: 0.0,
                    active: false,
                };
            };
            let channels = match meter.channel_count() {
                0 => usize::MAX,
                n => n,
            };
            let frames = std::iter::from_fn(|| updates.try_recv().ok());
            self.activity
                .take(frames.map(|frame| frame.peak.into_iter().take(channels)))
        }
    }

    fn start_obs(config: &HostConfig) -> Result<ObsContext, String> {
        // The OBS files sit next to this program, in cargo-obs-build's layout.
        let exe = std::env::current_exe()
            .map_err(|e| format!("the Modern engine cannot find itself: {e}"))?;
        let dir = exe
            .parent()
            .ok_or_else(|| "the Modern engine has no folder".to_string())?;
        let path = |relative: &str| ObsPath::new(&obs_path(&dir.join(relative)));
        let paths = StartupPaths::new(
            path("data/libobs"),
            path("obs-plugins/64bit"),
            path("data/obs-plugins/%module%"),
        );
        let even = |v: u32| (v & !1).max(2);
        let video = ObsVideoInfoBuilder::new()
            .base_width(config.canvas.0.max(2))
            .base_height(config.canvas.1.max(2))
            .output_width(even(config.output.0))
            .output_height(even(config.output.1))
            .fps_num(config.fps.max(1))
            .fps_den(1)
            .scale_type(ObsScaleType::Bicubic)
            .build();
        let audio = ObsAudioInfo::new(
            ObsSamplesPerSecond::F48000,
            ObsSpeakerLayout::Stereo,
            0,
            false,
        );
        StartupInfo::new()
            .set_startup_paths(paths)
            .set_module_config_path(ObsPath::new(&obs_path(&module_config_dir())))
            .set_video_info(video)
            .set_audio_info(audio)
            .set_logger(Box::new(logging::StderrLogger))
            .start()
            .map_err(|e| format!("The Modern engine did not start: {e}"))
    }

    /// Where OBS modules keep their settings, instead of next to the program.
    fn module_config_dir() -> PathBuf {
        let dir = std::env::var_os("APPDATA")
            .map(|appdata| PathBuf::from(appdata).join("Replayr").join("obs"))
            .unwrap_or_else(|| std::env::temp_dir().join("replayr-obs"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("cannot create {}: {e}", dir.display());
        }
        dir
    }

    fn audio_source(
        ctx: &ObsContext,
        index: usize,
        input: &AudioInput,
    ) -> Result<ObsSourceRef, ObsError> {
        let mut settings = ctx.data()?;
        let id = match input.kind {
            AudioKind::Output | AudioKind::Input => {
                settings.set_string("device_id", input.id.as_str())?;
                if input.kind == AudioKind::Output {
                    "wasapi_output_capture"
                } else {
                    "wasapi_input_capture"
                }
            }
            AudioKind::App => {
                settings
                    .set_string("window", window_id("", "", &input.id))?
                    .set_int(
                        "priority",
                        i64::from(obs_sys::window_priority_WINDOW_PRIORITY_EXE),
                    )?;
                "wasapi_process_output_capture"
            }
        };
        let source = ObsSourceRef::new(
            id,
            format!("audio {}", index + 1),
            Some(settings.into_immutable()),
            None,
            ctx.runtime().clone(),
        )?;
        let volume = if input.volume.is_finite() {
            input.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        source.set_volume(LinearVolume::new(volume)?)?;
        source.set_audio_mixes(AudioMixMask::from_bits(input.mixers & ((1 << TRACKS) - 1))?)?;
        Ok(source)
    }

    /// The watermark image, fitted into [`watermark_box`].
    fn add_watermark(
        ctx: &ObsContext,
        scene: &mut ObsSceneRef,
        file: &Path,
        config: &HostConfig,
    ) -> Result<ObsSceneItemRef<ObsSourceRef>, ObsError> {
        let mut settings = ctx.data()?;
        settings.set_string("file", obs_path(file))?;
        let source = ObsSourceRef::new(
            "image_source",
            "watermark",
            Some(settings.into_immutable()),
            None,
            ctx.runtime().clone(),
        )?;
        let item = scene.add(source)?;
        let b = watermark_box(config.canvas, config.output);
        let transform = ObsTransformInfoBuilder::new()
            .set_pos(Vec2::new(b.x, b.y))
            .set_alignment(ObsAlignment::LEFT | ObsAlignment::TOP)
            .set_bounds(Vec2::new(b.width, b.height))
            .set_bounds_type(ObsBoundsType::ScaleInner)
            .set_bounds_alignment(ObsAlignment::RIGHT | ObsAlignment::BOTTOM)
            .build(config.canvas.0, config.canvas.1);
        item.set_transform_info(&transform)?;
        item.set_scale_filter(ObsScaleType::Area)?;
        Ok(item)
    }

    /// The replay buffer and the recording output, with one AAC encoder per
    /// used track shared by both. The video encoder comes in `start_replay`.
    fn create_outputs(
        ctx: &mut ObsContext,
        config: &HostConfig,
        dir: &Path,
    ) -> Result<(ObsReplayBufferOutputRef, ObsOutputRef), ObsError> {
        let mut settings = ctx.data()?;
        settings
            .set_int("max_time_sec", i64::from(config.buffer_seconds.max(1)))?
            .set_int("max_size_mb", REPLAY_MAX_MB)?
            .set_string("directory", obs_path(dir))?
            .set_string("format", "Replay %CCYY-%MM-%DD %hh-%mm-%ss")?
            .set_string("extension", "mp4")?
            .set_bool("allow_spaces", true)?;
        let replay = ctx.replay_buffer(OutputInfo::new(
            "replay_buffer",
            "replay buffer",
            Some(settings),
            None,
        ))?;
        let recording = ctx.output(OutputInfo::new("ffmpeg_muxer", "recording", None, None))?;
        for (slot, track) in used_tracks(&config.audio).into_iter().enumerate() {
            let mut settings = ctx.data()?;
            settings.set_int("bitrate", AUDIO_KBPS)?;
            let info = AudioEncoderInfo::new(
                "ffmpeg_aac",
                format!("aac track {}", track + 1),
                Some(settings),
                None,
            );
            let encoder = ObsAudioEncoder::new_from_info_for_mix(
                info,
                AudioMixIndex::new(track)?,
                ctx.runtime().clone(),
            )?;
            let slot = AudioEncoderSlot::new(slot)?;
            replay.set_audio_encoder_at_slot(encoder.clone(), slot)?;
            recording.set_audio_encoder_at_slot(encoder, slot)?;
        }
        Ok((replay, recording))
    }

    fn video_encoder(
        ctx: &ObsContext,
        encoder: Encoder,
        config: &HostConfig,
    ) -> Result<Arc<ObsVideoEncoder>, ObsError> {
        let mut settings = ctx.data()?;
        settings
            .set_string("rate_control", "CBR")?
            .set_int("bitrate", i64::from(config.bitrate_kbps))?
            .set_int("keyint_sec", i64::from(config.keyint_sec.max(1)))?;
        let info = VideoEncoderInfo::new(encoder.obs_id(), encoder.obs_id(), Some(settings), None);
        ObsVideoEncoder::new_from_info(info, ctx.runtime().clone())
    }

    /// `obs_output_update` with one string setting. libobs-wrapper refuses
    /// updates while an output runs, but the replay buffer reads its folder
    /// only when it saves, so changing it while it runs is safe.
    fn set_output_string(
        output: &ObsReplayBufferOutputRef,
        key: &'static CStr,
        value: &str,
    ) -> Result<(), String> {
        let value = CString::new(value).map_err(|_| format!("{value:?} contains a NUL"))?;
        let handle = output.__native_handle();
        output
            .runtime()
            .run_with_obs_result(move || {
                // SAFETY: runs on the OBS thread; `handle` keeps the output
                // alive for the call; the data is made and released here and
                // OBS copies the strings it is given.
                unsafe {
                    let data = obs_sys::obs_data_create();
                    obs_sys::obs_data_set_string(data, key.as_ptr(), value.as_ptr());
                    obs_sys::obs_output_update(handle.raw_ptr_unchecked(), data);
                    obs_sys::obs_data_release(data);
                }
            })
            .map_err(|e| format!("cannot change the replay folder: {e}"))
    }

    fn create_dir(dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
    }

    /// OBS reports paths with `/`; the app gets them with `\`.
    fn native_path(path: &Path) -> PathBuf {
        path.components().collect()
    }

    fn error(message: String) -> Reply {
        Reply::Error { message }
    }

    fn context(what: &'static str) -> impl FnOnce(ObsError) -> String {
        move |e| format!("{what}: {e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obs_protocol::{AudioKind, AudioLevel, Reply};

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn watermark_matches_the_old_engine_at_1080p() {
        // encode::watermark::watermark_for(1920, 1080): 2 % margin (22 px),
        // 4.5 % of the height (49 px), right edge at 1920 - 22.
        let b = watermark_box((1920, 1080), (1920, 1080));
        assert_eq!(b.height, 49.0);
        assert_eq!(b.x + b.width, 1898.0);
        assert_eq!(b.y + b.height, 1058.0);
        assert_eq!(b.x, 22.0, "the box may use the whole width but the margins");
    }

    #[test]
    fn watermark_is_placed_in_output_pixels_and_scaled_to_the_canvas() {
        let b = watermark_box((2560, 1440), (1920, 1080));
        let s = 1440.0 / 1080.0;
        assert!(close(b.height, 49.0 * s), "{b:?}");
        assert!(close(b.x + b.width, 1898.0 * s), "{b:?}");
        assert!(close(b.y + b.height, 1058.0 * s), "{b:?}");
    }

    #[test]
    fn small_outputs_keep_a_readable_watermark() {
        let b = watermark_box((160, 90), (160, 90));
        assert_eq!(b.height, 18.0);
        assert_eq!(b.y + b.height, 88.0);
    }

    #[test]
    fn paths_for_obs_drop_the_verbatim_prefix() {
        let obs = |p: &str| obs_path(Path::new(p));
        assert_eq!(
            obs(r"\\?\C:\Program Files\Replayr\obs"),
            r"C:\Program Files\Replayr\obs"
        );
        assert_eq!(obs(r"\\?\UNC\server\share\obs"), r"\\server\share\obs");
        assert_eq!(obs(r"C:\Replayr\obs"), r"C:\Replayr\obs");
    }

    #[test]
    fn levels_are_linear_peaks_between_zero_and_one() {
        assert_eq!(level_from_db(0.0), 1.0);
        assert!(close(level_from_db(-6.0206), 0.5));
        assert_eq!(level_from_db(f32::NEG_INFINITY), 0.0);
        assert_eq!(level_from_db(6.0), 1.0);
        assert_eq!(level_from_db(f32::NAN), 0.0);
    }

    /// A file in a folder of its own under the temp folder.
    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("replayr-obs-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn stopped_error() -> Reply {
        Reply::Error {
            message: "the recording stopped by itself".into(),
        }
    }

    #[test]
    fn a_recording_that_stopped_by_itself_is_handed_over_first() {
        let path = temp_file("Recording 1.mp4", b"ftyp...");
        assert_eq!(
            recording_stopped_by_itself(path.clone()),
            vec![Reply::RecordingSaved { path }, stopped_error()]
        );
    }

    #[test]
    fn a_missing_or_empty_recording_is_only_an_error() {
        let empty = temp_file("Recording 2.mp4", b"");
        assert_eq!(recording_stopped_by_itself(empty), vec![stopped_error()]);
        let missing = std::env::temp_dir().join("replayr-obs-host-no-such-file.mp4");
        assert_eq!(recording_stopped_by_itself(missing), vec![stopped_error()]);
    }

    #[test]
    fn a_source_that_never_delivered_is_not_active() {
        let mut activity = AudioActivity::default();
        let none: [[f32; 2]; 0] = [];
        assert_eq!(
            activity.take(none),
            AudioLevel {
                level: 0.0,
                active: false
            }
        );
    }

    #[test]
    fn a_source_is_active_from_its_first_audio_on() {
        let mut activity = AudioActivity::default();
        let level = activity.take([[-20.0, -6.0206], [-40.0, -40.0]]);
        assert!(level.active);
        assert!(close(level.level, 0.5), "{level:?}");
        // Quiet ticks later: still the same, working source.
        let none: [[f32; 2]; 0] = [];
        assert_eq!(
            activity.take(none),
            AudioLevel {
                level: 0.0,
                active: true
            }
        );
    }

    #[test]
    fn silence_counts_as_delivered_audio() {
        let mut activity = AudioActivity::default();
        let level = activity.take([[f32::NEG_INFINITY]]);
        assert_eq!(
            level,
            AudioLevel {
                level: 0.0,
                active: true
            }
        );
    }

    #[test]
    fn peak_level_takes_the_loudest_and_is_zero_without_audio() {
        assert!(close(peak_level([-20.0, -6.0206, f32::NAN]), 0.5));
        assert_eq!(peak_level([]), 0.0);
        assert_eq!(peak_level([f32::NEG_INFINITY]), 0.0);
    }

    fn input(mixers: u32) -> AudioInput {
        AudioInput {
            kind: AudioKind::Output,
            id: "default".into(),
            volume: 1.0,
            mixers,
        }
    }

    #[test]
    fn track_one_is_always_encoded() {
        assert_eq!(used_tracks(&[]), vec![0]);
    }

    #[test]
    fn every_track_a_source_uses_is_encoded() {
        assert_eq!(used_tracks(&[input(0b11), input(0b101)]), vec![0, 1, 2]);
    }

    #[test]
    fn tracks_beyond_six_are_ignored() {
        assert_eq!(used_tracks(&[input(0b1100_0001)]), vec![0]);
        assert_eq!(TRACKS, 6);
    }
}
