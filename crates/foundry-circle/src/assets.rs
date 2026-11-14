//! Foundry asset uploads.
//!
//! A DM converts assets on their own machine with media-oxide and the native
//! client uploads the converted file. Nothing a client sends is trusted here:
//! the kind, the name, the content type and the bytes are re-validated against
//! the same media-oxide policy the client used, and the file is written without
//! ever replacing an existing one.
//!
//! [`store_asset`] takes no database and no async runtime, so the whole upload
//! contract is testable on its own.

use std::{
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};

use axum::http::StatusCode;
use media_oxide::foundry::{AssetKind, allowed_upload, sniff_matches, svg_is_safe};
use uuid::Uuid;

/// How much of the body the magic sniff needs. SVG is the exception: it is a
/// text document, so the whole body is checked for both its root tag and its
/// safety.
const SNIFF_HEAD: usize = 4 * 1024;

/// The one content type an upload may declare. A JSON or form body cannot reach
/// the handler, which is the CSRF guard next to the SameSite=Lax cookie.
const UPLOAD_CONTENT_TYPE: &str = "application/octet-stream";

/// Where uploaded assets land and how Foundry sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetConfig {
    /// Absolute directory Foundry serves as `<data>/Data/<prefix>`.
    pub directory: PathBuf,
    /// Data-relative prefix, e.g. `circle`; the response path is
    /// `<prefix>/<kind dir>/<name>`.
    pub prefix: String,
}

impl AssetConfig {
    /// `FOUNDRY_CIRCLE_ASSETS_DIR` and `FOUNDRY_CIRCLE_ASSETS_PREFIX`, read like
    /// the other service configuration. Both are required; uploads answer 503
    /// when either is missing rather than guessing a directory.
    pub fn from_env() -> Option<Self> {
        Self::new(
            std::env::var("FOUNDRY_CIRCLE_ASSETS_DIR").ok().as_deref(),
            std::env::var("FOUNDRY_CIRCLE_ASSETS_PREFIX")
                .ok()
                .as_deref(),
        )
    }

    /// The two settings, or `None` when one is missing or unusable. The
    /// directory must be absolute (the broker writes there directly) and the
    /// prefix must be a plain relative path.
    pub fn new(directory: Option<&str>, prefix: Option<&str>) -> Option<Self> {
        let directory = directory?.trim();
        let prefix = prefix?.trim().trim_matches('/');
        if !Path::new(directory).is_absolute()
            || prefix.is_empty()
            || prefix
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return None;
        }
        Some(Self {
            directory: PathBuf::from(directory),
            prefix: prefix.to_owned(),
        })
    }
}

/// Why an upload was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AssetError {
    #[error("unknown asset kind")]
    Kind,
    #[error("asset name is not allowed for this kind")]
    Name,
    #[error("content type must be application/octet-stream")]
    ContentType,
    #[error("asset body is empty")]
    Empty,
    #[error("asset content does not match its name")]
    Mismatch,
    #[error("asset already exists")]
    Exists,
    #[error("asset could not be stored: {0}")]
    Store(String),
}

impl AssetError {
    /// The JSON body code; the two-argument forms stay server-side.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Kind => "invalid_kind",
            Self::Name => "invalid_name",
            Self::ContentType => "unsupported_content_type",
            Self::Empty => "empty_body",
            Self::Mismatch => "content_mismatch",
            Self::Exists => "asset_exists",
            Self::Store(_) => "asset_store_failed",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::ContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Exists => StatusCode::CONFLICT,
            Self::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Kind | Self::Name | Self::Empty | Self::Mismatch => StatusCode::BAD_REQUEST,
        }
    }
}

/// Validate one upload and store it at `<directory>/<kind dir>/<name>`.
///
/// Returns the path Foundry serves, or the reason nothing was written. Blocking
/// filesystem work: callers run it off the async runtime.
pub fn store_asset(
    config: &AssetConfig,
    kind: &str,
    name: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<String, AssetError> {
    if !content_type.is_some_and(octet_stream) {
        return Err(AssetError::ContentType);
    }
    let kind = AssetKind::from_str(kind).map_err(|_| AssetError::Kind)?;
    if !allowed_upload(kind, name) {
        return Err(AssetError::Name);
    }
    if body.is_empty() {
        return Err(AssetError::Empty);
    }
    if name.ends_with(".svg") {
        // SVG is the one kind of upload that can carry script, so the whole
        // document is decoded and checked, not just its head.
        let text = std::str::from_utf8(body).map_err(|_| AssetError::Mismatch)?;
        if !sniff_matches(name, body) || !svg_is_safe(text) {
            return Err(AssetError::Mismatch);
        }
    } else if !sniff_matches(name, &body[..body.len().min(SNIFF_HEAD)]) {
        return Err(AssetError::Mismatch);
    }
    let directory = config.directory.join(kind.dir());
    std::fs::create_dir_all(&directory).map_err(store_error)?;
    // Foundry runs as another user: the umask must not hide what it serves.
    readable_by_others(&directory, 0o755).map_err(store_error)?;
    let (temp, mut file) = Temp::create(&directory).map_err(store_error)?;
    readable_by_others(temp.path(), 0o644).map_err(store_error)?;
    file.write_all(body).map_err(store_error)?;
    file.sync_all().map_err(store_error)?;
    drop(file);
    // Publishing never replaces an existing asset: the hard link fails when the
    // name is taken, so a DM cannot quietly overwrite a map mid-campaign.
    match std::fs::hard_link(temp.path(), directory.join(name)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(AssetError::Exists);
        }
        Err(error) => return Err(store_error(error)),
    }
    Ok(format!("{}/{}/{}", config.prefix, kind.dir(), name))
}

fn octet_stream(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(UPLOAD_CONTENT_TYPE))
}

/// Set `mode` explicitly; creation modes are filtered by the process umask.
fn readable_by_others(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn store_error(error: std::io::Error) -> AssetError {
    AssetError::Store(error.to_string())
}

/// An exclusively created temp file in the asset's own directory, so the final
/// link never crosses a filesystem. Removed on drop: a refused or cancelled
/// upload leaves no partial file behind.
struct Temp(PathBuf);

impl Temp {
    fn create(directory: &Path) -> std::io::Result<(Self, std::fs::File)> {
        let path = directory.join(format!(".upload-{}", Uuid::new_v4().simple()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let file = options.open(&path)?;
        Ok((Self(path), file))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself, so a failing test leaves nothing.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("foundry-circle-assets-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn config(&self) -> AssetConfig {
            AssetConfig::new(Some(self.0.to_str().unwrap()), Some("circle")).expect("configuration")
        }

        fn entries(&self, kind: &str) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.0.join(kind))
                .map(|entries| {
                    entries
                        .filter_map(|entry| Some(entry.ok()?.file_name().to_str()?.to_owned()))
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const OCTET: Option<&str> = Some("application/octet-stream");

    fn webp() -> Vec<u8> {
        let mut bytes = b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec();
        bytes.extend_from_slice(&[0; 8]);
        bytes
    }

    fn png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0; 8]);
        bytes
    }

    fn svg(inner: &str) -> Vec<u8> {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\">{inner}</svg>").into_bytes()
    }

    /// Foundry reads as another user, so a restrictive umask or an existing
    /// private directory must not leave the asset unreadable to it.
    #[cfg(unix)]
    #[test]
    fn published_assets_are_readable_by_the_foundry_user() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let tokens = scratch.0.join("tokens");
        std::fs::create_dir(&tokens).unwrap();
        std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o700)).unwrap();
        store_asset(&scratch.config(), "token", "guard.webp", OCTET, &webp()).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&tokens), 0o755);
        assert_eq!(mode(&tokens.join("guard.webp")), 0o644);
    }

    #[test]
    fn a_valid_upload_lands_under_the_prefix_and_kind_directory() {
        let scratch = Scratch::new();
        let path = store_asset(&scratch.config(), "map", "cave-1.webp", OCTET, &webp()).unwrap();
        assert_eq!(path, "circle/maps/cave-1.webp");
        assert_eq!(
            std::fs::read(scratch.0.join("maps/cave-1.webp")).unwrap(),
            webp()
        );
        // The published file is the only entry: no temp file survives.
        assert_eq!(scratch.entries("maps"), vec!["cave-1.webp"]);

        let art = svg("<rect width=\"1\" height=\"1\"/>");
        let path = store_asset(&scratch.config(), "artwork", "crest.svg", OCTET, &art).unwrap();
        assert_eq!(path, "circle/artwork/crest.svg");
        assert_eq!(
            std::fs::read(scratch.0.join("artwork/crest.svg")).unwrap(),
            art
        );
    }

    #[test]
    fn a_second_upload_never_replaces_the_first() {
        let scratch = Scratch::new();
        store_asset(&scratch.config(), "map", "cave-1.webp", OCTET, &webp()).unwrap();
        let mut other = webp();
        other[4] = 0xff;
        assert_eq!(
            store_asset(&scratch.config(), "map", "cave-1.webp", OCTET, &other),
            Err(AssetError::Exists)
        );
        assert_eq!(
            std::fs::read(scratch.0.join("maps/cave-1.webp")).unwrap(),
            webp()
        );
        assert_eq!(scratch.entries("maps"), vec!["cave-1.webp"]);
    }

    #[test]
    fn unknown_kinds_and_names_are_refused_before_anything_is_written() {
        let scratch = Scratch::new();
        assert_eq!(
            store_asset(&scratch.config(), "monster", "cave-1.webp", OCTET, &webp()),
            Err(AssetError::Kind)
        );
        for name in ["../x.webp", "Upper.webp", "cave 1.webp", "cave-1.exe"] {
            assert_eq!(
                store_asset(&scratch.config(), "map", name, OCTET, &webp()),
                Err(AssetError::Name),
                "{name}"
            );
        }
        assert!(!scratch.0.join("maps").exists());
    }

    #[test]
    fn only_an_octet_stream_body_is_accepted() {
        let scratch = Scratch::new();
        for content_type in [None, Some("image/webp"), Some("application/json")] {
            assert_eq!(
                store_asset(
                    &scratch.config(),
                    "map",
                    "cave-1.webp",
                    content_type,
                    &webp()
                ),
                Err(AssetError::ContentType),
                "{content_type:?}"
            );
        }
        // A parameterised value is still the right type.
        assert!(
            store_asset(
                &scratch.config(),
                "map",
                "cave-1.webp",
                Some("Application/Octet-Stream; charset=binary"),
                &webp()
            )
            .is_ok()
        );
    }

    #[test]
    fn the_bytes_must_match_the_extension() {
        let scratch = Scratch::new();
        assert_eq!(
            store_asset(&scratch.config(), "map", "cave-1.webp", OCTET, &png()),
            Err(AssetError::Mismatch)
        );
        assert_eq!(
            store_asset(&scratch.config(), "map", "cave-1.webp", OCTET, &[]),
            Err(AssetError::Empty)
        );
    }

    #[test]
    fn an_svg_carrying_script_is_refused() {
        let scratch = Scratch::new();
        for unsafe_svg in [
            svg("<script>alert(1)</script>"),
            svg("<rect onload=\"alert(1)\"/>"),
            svg("<use href=\"javascript:alert(1)\"/>"),
        ] {
            assert_eq!(
                store_asset(
                    &scratch.config(),
                    "artwork",
                    "crest.svg",
                    OCTET,
                    &unsafe_svg
                ),
                Err(AssetError::Mismatch)
            );
        }
        // SVG is text: invalid UTF-8 is refused even with the right root tag.
        let mut broken = svg("<rect/>");
        broken.extend_from_slice(b"\xff");
        assert_eq!(
            store_asset(&scratch.config(), "artwork", "crest.svg", OCTET, &broken),
            Err(AssetError::Mismatch)
        );
        assert_eq!(scratch.entries("artwork"), Vec::<String>::new());
    }

    #[test]
    fn the_configuration_needs_an_absolute_directory_and_a_plain_prefix() {
        assert_eq!(AssetConfig::new(None, Some("circle")), None);
        assert_eq!(AssetConfig::new(Some("/var/lib/assets"), None), None);
        assert_eq!(
            AssetConfig::new(Some("relative/assets"), Some("circle")),
            None
        );
        assert_eq!(
            AssetConfig::new(Some("/var/lib/assets"), Some("../etc")),
            None
        );
        assert_eq!(AssetConfig::new(Some("/var/lib/assets"), Some("//")), None);
        assert_eq!(
            AssetConfig::new(Some(" /var/lib/assets "), Some("/circle/")),
            Some(AssetConfig {
                directory: PathBuf::from("/var/lib/assets"),
                prefix: "circle".into(),
            })
        );
    }
}
