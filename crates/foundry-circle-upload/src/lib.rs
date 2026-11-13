//! Native client for the Foundry Circle asset upload API.
//!
//! The broker signs a desktop user in through the system browser (a loopback
//! redirect with PKCE), hands back a server session, and accepts converted
//! Foundry VTT assets over `PUT /api/v1/assets/{kind}/{file_name}`.
//!
//! Conversion policy lives in [`media_oxide::foundry`], so this client and the
//! broker agree on normalized names, target dimensions, and accepted
//! extensions. Only [`convert`] touches the filesystem and spawns `ffmpeg`;
//! [`upload`] and [`whoami`] are plain async requests.
//!
//! ```no_run
//! # async fn run() -> foundry_circle_upload::Result<()> {
//! use foundry_circle_upload::{login, save_session};
//!
//! let session = login("https://foundry.example.com").await?;
//! save_session(&session)?;
//! # Ok(())
//! # }
//! ```

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use media_oxide::foundry;
use reqwest::{Client, Response, StatusCode, Url, header, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

pub use media_oxide::foundry::Error as ConvertError;
pub use media_oxide::foundry::{AssetKind, AudioCodec, Options};

/// Largest asset body the broker accepts, in bytes. Checked locally so a
/// refused upload never crosses the network.
pub const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// How long a [`login`] waits for the browser callback.
/// How long one loopback connection may take to send its request line.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Connect timeout for every request; a slow 100 MiB upload gets no total
/// timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Convenience alias for this crate's errors.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong talking to the broker.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The stored or passed server URL is not a usable broker base URL.
    #[error("invalid server URL: {0}")]
    InvalidServer(String),
    /// Cleartext HTTP was requested for anything but the local machine.
    #[error("`{0}` must use https; http is allowed only for localhost and 127.0.0.1")]
    InsecureServer(String),
    /// No session file exists yet.
    #[error("no saved session; run `login --server <URL>` first")]
    NoSession,
    /// The login callback never arrived.
    #[error("login timed out waiting for the browser callback; run `login` again")]
    LoginTimeout,
    /// The sign-in failed: refused grant, bad callback, or unexpected response.
    #[error("login failed: {0}")]
    Login(String),
    /// The broker session expired or was revoked.
    #[error("the server session expired; run `login` again")]
    Expired,
    /// The asset already exists; the broker never overwrites.
    #[error("asset already exists on the server")]
    Exists,
    /// The signed-in account is not a GM or admin.
    #[error("this account may not upload assets; the broker requires a GM or admin")]
    Forbidden,
    /// Asset storage is not configured on the broker.
    #[error("asset storage is not configured on the broker")]
    NotConfigured,
    /// The broker rejected the request with a status and error code.
    #[error("the broker rejected the upload ({status}): {message}")]
    Rejected {
        /// HTTP status returned by the broker.
        status: u16,
        /// Broker error code, or the response body when there is none.
        message: String,
    },
    /// The file name would not pass the broker's upload validation.
    #[error(
        "`{0}` is not a valid asset name: use lowercase words joined by dashes with an extension the kind accepts"
    )]
    InvalidName(String),
    /// The file is larger than the broker accepts.
    #[error("the file is {size} bytes; the broker accepts at most {limit}")]
    TooLarge {
        /// File size in bytes.
        size: u64,
        /// Broker limit in bytes.
        limit: u64,
    },
    /// No per-user config directory is available.
    #[error("cannot find a config directory; set XDG_CONFIG_HOME or HOME")]
    NoConfigDir,
    /// Conversion failed; `ffmpeg` and `ffprobe` must be on `PATH`.
    #[error("media conversion failed: {0}")]
    Convert(#[from] ConvertError),
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A JSON response could not be parsed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The request itself failed: DNS, TLS, connection reset, timeout.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

/// A broker session created by [`login`].
///
/// Treat the token like a password. It is stored with mode 0600 on unix and
/// stays valid until it expires or an admin revokes it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Session {
    /// Broker base URL, without a trailing slash.
    pub server: String,
    /// Cookie name the token is sent under.
    pub cookie: String,
    /// Opaque session token.
    pub token: String,
    /// Expiry reported by the broker.
    pub expires_at: String,
}

/// Sign in through the system browser and return the session.
///
/// Prints the authorization URL as well, so the sign-in still works on a
/// machine where no browser opens. This is the CLI entry point; a GUI should
/// call [`login_with`] and show the URL itself.
pub async fn login(server: &str) -> Result<Session> {
    login_with(server, |url| {
        println!("Open this URL to sign in:\n{url}\n");
        if let Err(error) = open_browser(url) {
            eprintln!("could not open a browser ({error}); open the URL above manually");
        }
    })
    .await
}

/// Sign in, handing the authorization URL to `opener` instead of a browser.
///
/// `opener` must return promptly: the loopback listener only answers once
/// control comes back to this future. Spawn the browser work on another task.
pub async fn login_with<F>(server: &str, opener: F) -> Result<Session>
where
    F: FnOnce(&str),
{
    let server = normalize_server(server)?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let state = random_token()?;
    let verifier = random_token()?;
    let authorize = format!(
        "{server}/api/auth/native?port={port}&state={state}&challenge={}",
        pkce_challenge(&verifier)
    );
    opener(&authorize);
    let code = timeout(LOGIN_TIMEOUT, accept_callback(&listener, &state))
        .await
        .map_err(|_| Error::LoginTimeout)??;
    exchange(&server, &code, &verifier).await
}

/// The signed-in principal, exactly as the broker describes it.
pub async fn whoami(session: &Session) -> Result<serde_json::Value> {
    let request = client()?.get(format!("{}/api/v1/me", session.server));
    let response = authorized(session, request).await?;
    if !response.status().is_success() {
        return Err(rejection(response).await);
    }
    Ok(response.json().await?)
}

/// Convert `input` for upload into `staging`, returning the converted path.
///
/// A thin wrapper over [`media_oxide::foundry::convert`]: the output lands in
/// `staging/<kind dir>/<normalized name>`, an existing output is an error (never
/// an overwrite), and `ffmpeg` plus `ffprobe` must be on `PATH`. Blocking, so a
/// GUI should run it on a blocking task.
pub fn convert(
    input: &Path,
    kind: Option<AssetKind>,
    opts: &Options,
    staging: &Path,
) -> Result<PathBuf> {
    Ok(foundry::convert(input, kind, opts, staging)?)
}

/// The kind and Foundry data-relative path (`tokens/guard-red.webp`) of a file
/// [`convert`] produced inside `staging`.
///
/// The kind can differ from the requested one: without an explicit kind,
/// conversion picks the format default (a PNG becomes `artwork`, audio becomes
/// `audio`). `None` means the path is not a converted asset.
pub fn staged_asset(staging: &Path, converted: &Path) -> Option<(AssetKind, String)> {
    const KINDS: [AssetKind; 5] = [
        AssetKind::Token,
        AssetKind::Map,
        AssetKind::Tile,
        AssetKind::Artwork,
        AssetKind::Audio,
    ];
    let rest = converted.strip_prefix(staging).ok()?;
    let dir = rest.components().next()?.as_os_str().to_str()?;
    let name = rest.components().nth(1)?.as_os_str().to_str()?;
    let kind = KINDS.into_iter().find(|kind| kind.dir() == dir)?;
    Some((kind, format!("{dir}/{name}")))
}

/// Upload one converted asset, returning the Foundry data-relative path.
///
/// The broker refuses names [`media_oxide::foundry::allowed_upload`] rejects and
/// bodies over [`MAX_UPLOAD_BYTES`]; both are checked here first.
pub async fn upload(session: &Session, kind: AssetKind, file: &Path) -> Result<String> {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::InvalidName(file.display().to_string()))?;
    if !foundry::allowed_upload(kind, name) {
        return Err(Error::InvalidName(name.to_string()));
    }
    let size = tokio::fs::metadata(file).await?.len();
    if size > MAX_UPLOAD_BYTES {
        return Err(Error::TooLarge {
            size,
            limit: MAX_UPLOAD_BYTES,
        });
    }
    let bytes = tokio::fs::read(file).await?;
    let request = client()?
        .put(format!("{}/api/v1/assets/{kind}/{name}", session.server))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(bytes);
    let response = authorized(session, request).await?;
    let status = response.status();
    if status.is_success() {
        #[derive(Deserialize)]
        struct Uploaded {
            path: String,
        }
        return Ok(response.json::<Uploaded>().await?.path);
    }
    Err(match status {
        StatusCode::CONFLICT => Error::Exists,
        StatusCode::FORBIDDEN => Error::Forbidden,
        _ => rejection(response).await,
    })
}

/// Validate and normalize a broker URL.
///
/// Trailing slashes are trimmed. `https` is required, except for `localhost`
/// and `127.0.0.1`, so a broker running on this machine needs no TLS.
pub fn normalize_server(server: &str) -> Result<String> {
    let trimmed = server.trim().trim_end_matches('/');
    let url = Url::parse(trimmed)
        .map_err(|error| Error::InvalidServer(format!("`{server}`: {error}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidServer(format!("`{server}` has no host")))?;
    match url.scheme() {
        "https" => {}
        "http" if host == "localhost" || host == "127.0.0.1" => {}
        "http" => return Err(Error::InsecureServer(server.to_string())),
        scheme => {
            return Err(Error::InvalidServer(format!(
                "`{server}` uses unsupported scheme `{scheme}`"
            )));
        }
    }
    if !url.username().is_empty() || url.query().is_some() || url.fragment().is_some() {
        return Err(Error::InvalidServer(format!(
            "`{server}` must not carry credentials, a query, or a fragment"
        )));
    }
    Ok(trimmed.to_string())
}

/// Path of the stored session file, if the platform has a config directory.
pub fn session_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("foundry-circle").join("session.json"))
}

/// Read the stored session, or `None` when nobody signed in yet.
pub fn load_session() -> Result<Option<Session>> {
    let Some(path) = session_path() else {
        return Ok(None);
    };
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Store the session for later commands, readable only by this user on unix.
pub fn save_session(session: &Session) -> Result<()> {
    let path = session_path().ok_or(Error::NoConfigDir)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_private(&path, &serde_json::to_vec_pretty(session)?)?;
    Ok(())
}

/// Delete the stored session file. The server session stays valid until it
/// expires or an admin revokes it.
pub fn forget_session() -> Result<()> {
    let Some(path) = session_path() else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// A fresh 32-byte random value as unpadded base64url: 43 characters, the
/// length PKCE requires for a verifier.
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| Error::Login(format!("system randomness unavailable: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// `base64url(sha256(verifier))`, PKCE S256.
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("foundry-circle-upload/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Send `request` with the session cookie, mapping a login redirect to
/// [`Error::Expired`].
async fn authorized(session: &Session, request: reqwest::RequestBuilder) -> Result<Response> {
    let response = request
        .header(
            header::COOKIE,
            format!("{}={}", session.cookie, session.token),
        )
        .send()
        .await?;
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if response.status() == StatusCode::UNAUTHORIZED
        || (response.status().is_redirection() && location.contains("/api/auth/login"))
    {
        return Err(Error::Expired);
    }
    Ok(response)
}

/// Map a non-success response to the narrowest error the broker reports.
async fn rejection(response: Response) -> Error {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| json.get("error")?.as_str().map(str::to_string))
    {
        Some(code) if code == "assets_not_configured" => Error::NotConfigured,
        Some(code) => Error::Rejected {
            status,
            message: code,
        },
        None => Error::Rejected {
            status,
            message: body.trim().to_string(),
        },
    }
}

#[derive(Serialize)]
struct Exchange<'a> {
    code: &'a str,
    verifier: &'a str,
}

#[derive(Deserialize)]
struct Issued {
    token: String,
    cookie: String,
    expires_at: String,
}

/// Trade the callback code for a session, sending the PKCE verifier.
async fn exchange(server: &str, code: &str, verifier: &str) -> Result<Session> {
    let response = client()?
        .post(format!("{server}/api/auth/native/token"))
        .json(&Exchange { code, verifier })
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        let reason = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|json| json.get("error")?.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{status}"));
        return Err(Error::Login(format!(
            "the broker refused the sign-in ({reason})"
        )));
    }
    let issued: Issued = serde_json::from_str(&body)?;
    Ok(Session {
        server: server.to_string(),
        cookie: issued.cookie,
        token: issued.token,
        expires_at: issued.expires_at,
    })
}

/// Why a request line is not this login's callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallbackIssue {
    /// A different path, such as a browser's `/favicon.ico` probe.
    NotCallback,
    /// `/callback` without a usable `code` or `state`.
    BadRequest,
    /// The `state` does not match this login attempt.
    StateMismatch,
}

impl CallbackIssue {
    fn status(self) -> u16 {
        match self {
            Self::NotCallback => 404,
            Self::BadRequest | Self::StateMismatch => 400,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::NotCallback => "Not the sign-in callback.",
            Self::BadRequest => "The sign-in callback was incomplete. Start the login again.",
            Self::StateMismatch => "The sign-in state did not match. Start the login again.",
        }
    }
}

impl std::fmt::Display for CallbackIssue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Read the code out of `GET /callback?code=..&state=.. HTTP/1.1`.
fn callback_code(
    request_line: &str,
    expected_state: &str,
) -> std::result::Result<String, CallbackIssue> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method != "GET" || path != "/callback" {
        return Err(CallbackIssue::NotCallback);
    }
    let (mut code, mut state) = (None, None);
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            _ => {}
        }
    }
    let code = code
        .filter(|value| !value.is_empty())
        .ok_or(CallbackIssue::BadRequest)?;
    let state = state.ok_or(CallbackIssue::BadRequest)?;
    if state != expected_state {
        return Err(CallbackIssue::StateMismatch);
    }
    Ok(code.to_string())
}

/// Answer loopback connections until one of them is this login's callback.
async fn accept_callback(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _peer) = listener.accept().await?;
        // A browser probe or an idle preconnection is not our callback; bound
        // each read so it cannot hold the listener until the login times out.
        let line = tokio::time::timeout(REQUEST_READ_TIMEOUT, read_request_line(&mut stream))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        match callback_code(&line, expected_state) {
            Ok(code) => {
                let _ = respond(&mut stream, 200, "Signed in. You can close this tab.").await;
                return Ok(code);
            }
            Err(CallbackIssue::NotCallback) => {
                let _ = respond(&mut stream, 404, CallbackIssue::NotCallback.message()).await;
            }
            Err(issue) => {
                let _ = respond(&mut stream, issue.status(), issue.message()).await;
                return Err(Error::Login(issue.to_string()));
            }
        }
    }
}

/// Read up to the first request line; the callback never sends a body.
async fn read_request_line(stream: &mut TcpStream) -> Result<String> {
    let mut buffer = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(2).any(|window| window == b"\r\n") || buffer.len() >= 4096 {
            break;
        }
    }
    let line = buffer
        .split(|byte| *byte == b'\n' || *byte == b'\r')
        .next()
        .unwrap_or_default();
    Ok(String::from_utf8_lossy(line).trim().to_string())
}

/// A minimal HTML page, because the browser tab is the only UI here.
async fn respond(stream: &mut TcpStream, status: u16, message: &str) -> Result<()> {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Foundry Circle</title></head><body><p>{message}</p></body></html>"
    );
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Start the opener and return at once: `xdg-open` can stay attached to the
/// browser, and the callback listener must already be serving.
fn open_browser(url: &str) -> std::io::Result<()> {
    browser_command()
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

/// No shell: `cmd /C start` would read the URL's `&` as a command separator.
#[cfg(target_os = "windows")]
fn browser_command() -> std::process::Command {
    let mut command = std::process::Command::new("rundll32");
    command.arg("url.dll,FileProtocolHandler");
    command
}

#[cfg(target_os = "macos")]
fn browser_command() -> std::process::Command {
    std::process::Command::new("open")
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn browser_command() -> std::process::Command {
    std::process::Command::new("xdg-open")
}

#[cfg(target_os = "windows")]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B. The briefing's pair (verifier ending `...7wW1gFrWuj7cA`,
    /// challenge `E9Melhoa2OwvFVMqeIXy1lbaxH10Gd2YRXhn5RU-zh0`) is not the
    /// RFC's: hashing that verifier gives `s0u6LDrHrjfeGQEY57VNJC8u8CAq-...`.
    #[test]
    fn pkce_matches_the_rfc_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_tokens_are_verifier_shaped() {
        let token = random_token().expect("randomness");
        assert_eq!(token.len(), 43);
        assert_ne!(random_token().expect("randomness"), token);
    }

    #[test]
    fn callback_accepts_a_matching_state() {
        let line = "GET /callback?code=abc123&state=xyz HTTP/1.1";
        assert_eq!(callback_code(line, "xyz"), Ok("abc123".to_string()));
    }

    #[test]
    fn callback_ignores_other_paths() {
        let line = "GET /favicon.ico HTTP/1.1";
        assert_eq!(callback_code(line, "xyz"), Err(CallbackIssue::NotCallback));
        let line = "GET /callback/extra?code=abc&state=xyz HTTP/1.1";
        assert_eq!(callback_code(line, "xyz"), Err(CallbackIssue::NotCallback));
        assert_eq!(callback_code("", "xyz"), Err(CallbackIssue::NotCallback));
    }

    #[test]
    fn callback_requires_code_and_state() {
        assert_eq!(
            callback_code("GET /callback?state=xyz HTTP/1.1", "xyz"),
            Err(CallbackIssue::BadRequest)
        );
        assert_eq!(
            callback_code("GET /callback?code=&state=xyz HTTP/1.1", "xyz"),
            Err(CallbackIssue::BadRequest)
        );
        assert_eq!(
            callback_code("GET /callback?code=abc HTTP/1.1", "xyz"),
            Err(CallbackIssue::BadRequest)
        );
    }

    #[test]
    fn callback_rejects_a_foreign_state() {
        assert_eq!(
            callback_code("GET /callback?code=abc&state=other HTTP/1.1", "xyz"),
            Err(CallbackIssue::StateMismatch)
        );
    }

    #[test]
    fn server_urls_are_validated_and_trimmed() {
        assert_eq!(
            normalize_server("https://foundry.example.com/").expect("https"),
            "https://foundry.example.com"
        );
        assert_eq!(
            normalize_server("https://foundry.example.com/base/").expect("subpath"),
            "https://foundry.example.com/base"
        );
        assert_eq!(
            normalize_server("http://localhost:8032").expect("loopback"),
            "http://localhost:8032"
        );
        assert_eq!(
            normalize_server("http://127.0.0.1:8032").expect("loopback ip"),
            "http://127.0.0.1:8032"
        );
        assert!(matches!(
            normalize_server("http://foundry.example.com"),
            Err(Error::InsecureServer(_))
        ));
        assert!(matches!(
            normalize_server("ftp://foundry.example.com"),
            Err(Error::InvalidServer(_))
        ));
        assert!(matches!(
            normalize_server("foundry.example.com"),
            Err(Error::InvalidServer(_))
        ));
    }

    #[test]
    fn staged_assets_map_back_to_their_kind() {
        let staging = Path::new("/staging");
        assert_eq!(
            staged_asset(staging, Path::new("/staging/tokens/guard-red.webp")),
            Some((AssetKind::Token, "tokens/guard-red.webp".to_string()))
        );
        assert_eq!(
            staged_asset(staging, Path::new("/staging/elsewhere/x.png")),
            None
        );
        assert_eq!(staged_asset(staging, Path::new("/staging/tokens")), None);
    }
}
