//! Pure state and policy for the uploader window.
//!
//! Everything here is renderer-free so it can be unit tested: the option
//! mapping and ranges the Dioxus view renders, the queue's status vocabulary,
//! and the wording of the two errors the window treats specially.

use std::path::PathBuf;

use foundry_circle_upload::{AssetKind, AudioCodec, Error, Options, Session};

/// Token art is square and Foundry's guides ask for 200-400 px.
pub const TOKEN_PX_MIN: u32 = 200;
pub const TOKEN_PX_MAX: u32 = 400;
pub const TOKEN_PX_DEFAULT: u32 = 400;
/// One map grid cell, in pixels.
pub const GRID_PX_DEFAULT: u32 = 100;
/// Lossless masters are re-encoded below this range; above it nothing is gained.
pub const AUDIO_KBPS_MIN: u32 = 64;
pub const AUDIO_KBPS_MAX: u32 = 192;
pub const AUDIO_KBPS_DEFAULT: u32 = 128;

/// The asset kinds the picker offers. `Auto` lets media-oxide apply its format
/// default, which is what the CLI does when `--kind` is omitted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KindChoice {
    #[default]
    Auto,
    Token,
    Map,
    Tile,
    Artwork,
    Audio,
}

impl KindChoice {
    pub const ALL: [KindChoice; 6] = [
        KindChoice::Auto,
        KindChoice::Token,
        KindChoice::Map,
        KindChoice::Tile,
        KindChoice::Artwork,
        KindChoice::Audio,
    ];

    pub fn value(self) -> &'static str {
        match self {
            KindChoice::Auto => "auto",
            KindChoice::Token => "token",
            KindChoice::Map => "map",
            KindChoice::Tile => "tile",
            KindChoice::Artwork => "artwork",
            KindChoice::Audio => "audio",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            KindChoice::Auto => "Auto (by file format)",
            KindChoice::Token => "Token",
            KindChoice::Map => "Map",
            KindChoice::Tile => "Tile",
            KindChoice::Artwork => "Artwork",
            KindChoice::Audio => "Audio",
        }
    }

    /// `None` is the auto choice; conversion then picks the format default.
    pub fn kind(self) -> Option<AssetKind> {
        match self {
            KindChoice::Auto => None,
            KindChoice::Token => Some(AssetKind::Token),
            KindChoice::Map => Some(AssetKind::Map),
            KindChoice::Tile => Some(AssetKind::Tile),
            KindChoice::Artwork => Some(AssetKind::Artwork),
            KindChoice::Audio => Some(AssetKind::Audio),
        }
    }

    pub fn from_value(value: &str) -> Self {
        KindChoice::ALL
            .into_iter()
            .find(|choice| choice.value() == value)
            .unwrap_or_default()
    }
}

pub fn codec_value(codec: AudioCodec) -> &'static str {
    match codec {
        AudioCodec::Vorbis => "vorbis",
        AudioCodec::Opus => "opus",
    }
}

pub fn codec_label(codec: AudioCodec) -> &'static str {
    match codec {
        AudioCodec::Vorbis => "Vorbis (broadest support)",
        AudioCodec::Opus => "Opus (smaller files)",
    }
}

pub fn codec_from_value(value: &str) -> AudioCodec {
    match value {
        "opus" => AudioCodec::Opus,
        _ => AudioCodec::Vorbis,
    }
}

/// The conversion controls, as the window holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiOptions {
    pub kind: KindChoice,
    pub token_px: u32,
    pub grid_px: u32,
    pub audio: AudioCodec,
    pub audio_kbps: u32,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            kind: KindChoice::default(),
            token_px: TOKEN_PX_DEFAULT,
            grid_px: GRID_PX_DEFAULT,
            audio: AudioCodec::Vorbis,
            audio_kbps: AUDIO_KBPS_DEFAULT,
        }
    }
}

impl UiOptions {
    /// The library options for one conversion. Out-of-range values are clamped
    /// here as well as in the inputs, so a typed value can never reach ffmpeg.
    pub fn conversion(self) -> Options {
        Options {
            token_px: clamp_token_px(self.token_px),
            grid_px: if self.grid_px == 0 {
                GRID_PX_DEFAULT
            } else {
                self.grid_px
            },
            audio: self.audio,
            audio_kbps: clamp_audio_kbps(self.audio_kbps),
        }
    }
}

pub fn clamp_token_px(value: u32) -> u32 {
    value.clamp(TOKEN_PX_MIN, TOKEN_PX_MAX)
}

pub fn clamp_audio_kbps(value: u32) -> u32 {
    value.clamp(AUDIO_KBPS_MIN, AUDIO_KBPS_MAX)
}

/// One file's place in the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Converting,
    Uploading,
    Done,
    Exists,
    Failed,
}

/// One queued file: its path, the name the table shows, and where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub path: PathBuf,
    pub name: String,
    pub status: JobStatus,
    /// The Foundry path once `Done`, or why it failed.
    pub detail: Option<String>,
}

impl Job {
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            name,
            status: JobStatus::Queued,
            detail: None,
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self.status {
            JobStatus::Queued => "Queued",
            JobStatus::Converting => "Converting",
            JobStatus::Uploading => "Uploading",
            JobStatus::Done => "Done",
            JobStatus::Exists => "Exists",
            JobStatus::Failed => "Failed",
        }
    }

    /// The Foundry path for a finished upload, which is what the Copy button
    /// takes. `None` for every other state.
    pub fn foundry_path(&self) -> Option<&str> {
        match self.status {
            JobStatus::Done => self.detail.as_deref(),
            _ => None,
        }
    }
}

/// What the sign-in panel prefills: the saved server, or nothing for a first run.
pub fn server_prefill(saved: Option<&Session>) -> String {
    saved
        .map(|session| session.server.clone())
        .unwrap_or_default()
}

/// `display_name (subject)` from `GET /api/v1/me`, with the camelCase key the
/// broker sends and the snake_case fallback the CLI also accepts.
pub fn account_label(principal: &serde_json::Value) -> String {
    let display = principal
        .get("displayName")
        .or_else(|| principal.get("display_name"))
        .and_then(serde_json::Value::as_str);
    let subject = principal.get("subject").and_then(serde_json::Value::as_str);
    match (display, subject) {
        (Some(display), Some(subject)) => format!("{display} ({subject})"),
        (Some(display), None) => display.to_string(),
        (None, Some(subject)) => subject.to_string(),
        (None, None) => "signed in".to_string(),
    }
}

/// The wording for an error, and whether it ends the session.
pub fn describe_error(error: &Error) -> (String, bool) {
    match error {
        Error::Expired => (
            "The server session expired. Sign in again.".to_string(),
            true,
        ),
        Error::Forbidden => ("this account is not a GM or admin".to_string(), false),
        other => (other.to_string(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_choices_round_trip_through_the_select_value() {
        for choice in KindChoice::ALL {
            assert_eq!(KindChoice::from_value(choice.value()), choice);
        }
        assert_eq!(KindChoice::from_value("nonsense"), KindChoice::Auto);
        assert_eq!(KindChoice::Auto.kind(), None);
        assert_eq!(KindChoice::Token.kind(), Some(AssetKind::Token));
        assert_eq!(KindChoice::Audio.kind(), Some(AssetKind::Audio));
    }

    #[test]
    fn codecs_round_trip_through_the_select_value() {
        for codec in [AudioCodec::Vorbis, AudioCodec::Opus] {
            assert_eq!(codec_from_value(codec_value(codec)), codec);
        }
        assert_eq!(codec_from_value("nonsense"), AudioCodec::Vorbis);
    }

    #[test]
    fn conversion_options_are_clamped() {
        let low = UiOptions {
            token_px: 10,
            grid_px: 0,
            audio_kbps: 1,
            ..UiOptions::default()
        };
        let options = low.conversion();
        assert_eq!(options.token_px, TOKEN_PX_MIN);
        assert_eq!(options.grid_px, GRID_PX_DEFAULT);
        assert_eq!(options.audio_kbps, AUDIO_KBPS_MIN);

        let high = UiOptions {
            token_px: 9000,
            audio_kbps: 9000,
            ..UiOptions::default()
        };
        let options = high.conversion();
        assert_eq!(options.token_px, TOKEN_PX_MAX);
        assert_eq!(options.audio_kbps, AUDIO_KBPS_MAX);

        // The defaults are already inside the policy.
        assert_eq!(UiOptions::default().conversion(), Options::default());
    }

    #[test]
    fn the_server_field_prefills_the_saved_session() {
        assert_eq!(server_prefill(None), "");
        let session = Session {
            server: "https://foundry.example.test".into(),
            cookie: "foundry_circle_session".into(),
            token: "secret".into(),
            expires_at: "2026-11-13T00:00:00Z".into(),
        };
        assert_eq!(
            server_prefill(Some(&session)),
            "https://foundry.example.test"
        );
    }

    #[test]
    fn the_account_label_prefers_the_broker_display_name() {
        let principal = serde_json::json!({
            "subject": "abc",
            "displayName": "Robin",
        });
        assert_eq!(account_label(&principal), "Robin (abc)");
        assert_eq!(account_label(&serde_json::json!({"subject": "abc"})), "abc");
        assert_eq!(account_label(&serde_json::json!({})), "signed in");
    }

    #[test]
    fn an_expired_session_signs_out_and_a_forbidden_one_stays() {
        let (message, sign_out) = describe_error(&Error::Expired);
        assert!(message.contains("Sign in again"));
        assert!(sign_out);
        let (message, sign_out) = describe_error(&Error::Forbidden);
        assert_eq!(message, "this account is not a GM or admin");
        assert!(!sign_out);
    }

    #[test]
    fn jobs_name_themselves_and_expose_only_finished_paths() {
        let job = Job::new(PathBuf::from("/tmp/guard-red.png"));
        assert_eq!(job.name, "guard-red.png");
        assert_eq!(job.status_label(), "Queued");
        assert_eq!(job.foundry_path(), None);

        let done = Job {
            status: JobStatus::Done,
            detail: Some("circle/tokens/guard-red.webp".into()),
            ..job.clone()
        };
        assert_eq!(done.status_label(), "Done");
        assert_eq!(done.foundry_path(), Some("circle/tokens/guard-red.webp"));

        let failed = Job {
            status: JobStatus::Failed,
            detail: Some("ffmpeg: not found".into()),
            ..job
        };
        assert_eq!(failed.status_label(), "Failed");
        assert_eq!(failed.foundry_path(), None);
    }
}
