//! Messages between the app and the OBS helper process (`replayr-obs.exe`).
//! One JSON message per line over stdin/stdout.
//!
//! Replayr has its own copy of this crate. Change both together, with a new
//! helper version.

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// App to helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Configure(HostConfig),
    Save { dir: PathBuf },
    RecordStart { dir: PathBuf },
    RecordStop,
    Quit,
}

/// Helper to app. Adjacently tagged because `Levels` holds a sequence, which
/// an internal tag cannot carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Reply {
    Ready,
    Hooked {
        capture: CaptureKind,
    },
    Saved {
        path: PathBuf,
    },
    RecordingSaved {
        path: PathBuf,
    },
    /// One per `HostConfig.audio` entry, in that order.
    Levels(Vec<AudioLevel>),
    EncoderFallback {
        from: Encoder,
        error: String,
    },
    Error {
        message: String,
    },
}

/// One audio source in `Reply::Levels`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioLevel {
    /// The peak since the last `Levels`, after volume, 0..=1.
    pub level: f32,
    /// OBS made the source and it has delivered audio since it started. A
    /// per-app source whose app OBS cannot find (OBS looks for a visible
    /// window, so an app closed to the tray) never delivers.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostConfig {
    pub canvas: (u32, u32),
    pub output: (u32, u32),
    pub fps: u32,
    pub encoder: Encoder,
    pub bitrate_kbps: u32,
    pub keyint_sec: u32,
    pub buffer_seconds: u32,
    pub source: Source,
    pub audio: Vec<AudioInput>,
    pub watermark: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Window {
        title: String,
        class: String,
        exe: String,
    },
    Monitor {
        index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    Game,
    Window,
    Monitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoder {
    Nvenc,
    Amf,
    Qsv,
    X264,
}

impl Encoder {
    /// The OBS encoder id (OBS 32).
    pub fn obs_id(self) -> &'static str {
        match self {
            Encoder::Nvenc => "obs_nvenc_h264_tex",
            Encoder::Amf => "h264_texture_amf",
            Encoder::Qsv => "obs_qsv11_v2",
            Encoder::X264 => "obs_x264",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioInput {
    pub kind: AudioKind,
    pub id: String,
    pub volume: f32,
    /// Bitmask of the tracks this source goes to; bit 0 is track 1.
    pub mixers: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioKind {
    Output,
    Input,
    App,
}

/// One line of JSON, ending in `\n`.
pub fn encode(msg: &impl Serialize) -> String {
    let mut line = serde_json::to_string(msg).expect("protocol messages serialize");
    line.push('\n');
    line
}

pub fn decode<T: DeserializeOwned>(line: &str) -> Result<T, String> {
    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> HostConfig {
        HostConfig {
            canvas: (2560, 1440),
            output: (1920, 1080),
            fps: 60,
            encoder: Encoder::Nvenc,
            bitrate_kbps: 12000,
            keyint_sec: 1,
            buffer_seconds: 60,
            source: Source::Window {
                title: "Game\nwith \"quotes\"".into(),
                class: "UnrealWindow".into(),
                exe: "game.exe".into(),
            },
            audio: vec![
                AudioInput {
                    kind: AudioKind::Output,
                    id: "default".into(),
                    volume: 0.5,
                    mixers: 0b11,
                },
                AudioInput {
                    kind: AudioKind::App,
                    id: "discord.exe".into(),
                    volume: 1.0,
                    mixers: 0b101,
                },
            ],
            watermark: Some(PathBuf::from("wm.png")),
        }
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(msg: T) {
        let line = encode(&msg);
        assert_eq!(decode::<T>(&line).unwrap(), msg, "{line}");
    }

    #[test]
    fn every_request_round_trips() {
        round_trip(Request::Configure(config()));
        let mut monitor = config();
        monitor.source = Source::Monitor { index: 1 };
        monitor.watermark = None;
        round_trip(Request::Configure(monitor));
        round_trip(Request::Save {
            dir: "clips".into(),
        });
        round_trip(Request::RecordStart { dir: "rec".into() });
        round_trip(Request::RecordStop);
        round_trip(Request::Quit);
    }

    #[test]
    fn every_reply_round_trips() {
        round_trip(Reply::Ready);
        for capture in [CaptureKind::Game, CaptureKind::Window, CaptureKind::Monitor] {
            round_trip(Reply::Hooked { capture });
        }
        round_trip(Reply::Saved {
            path: "a.mp4".into(),
        });
        round_trip(Reply::RecordingSaved {
            path: "b.mp4".into(),
        });
        round_trip(Reply::Levels(vec![
            AudioLevel {
                level: 0.0,
                active: false,
            },
            AudioLevel {
                level: 0.25,
                active: true,
            },
        ]));
        round_trip(Reply::EncoderFallback {
            from: Encoder::Amf,
            error: "no device".into(),
        });
        round_trip(Reply::Error {
            message: "boom".into(),
        });
    }

    #[test]
    fn levels_carry_a_level_and_whether_the_source_delivers() {
        let line = encode(&Reply::Levels(vec![AudioLevel {
            level: 0.5,
            active: true,
        }]));
        assert_eq!(
            line,
            "{\"type\":\"levels\",\"data\":[{\"level\":0.5,\"active\":true}]}\n"
        );
    }

    #[test]
    fn encode_is_one_line() {
        let line = encode(&Request::Configure(config()));
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
    }

    #[test]
    fn obs_ids_match_obs_32() {
        assert_eq!(Encoder::Nvenc.obs_id(), "obs_nvenc_h264_tex");
        assert_eq!(Encoder::Amf.obs_id(), "h264_texture_amf");
        assert_eq!(Encoder::Qsv.obs_id(), "obs_qsv11_v2");
        assert_eq!(Encoder::X264.obs_id(), "obs_x264");
    }
}
