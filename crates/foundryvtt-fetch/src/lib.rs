//! Credential-safe Foundry VTT release acquisition.
//!
//! This crate intentionally owns only acquisition and validation.  Nix update
//! tooling consumes [`Artifact::provenance`] to validate the exact hash selected
//! by nix-foundryvtt; Nix evaluation/builds never contact foundryvtt.com.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroizing;
use zip::ZipArchive;

pub mod reconcile;

const DEFAULT_SITE: &str = "https://foundryvtt.com";
const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 100_000;
const MAX_PACKAGE_JSON_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("invalid release identifier: {0}")]
    InvalidRelease(String),
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("archive is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("archive exceeds the configured size limit ({limit} bytes)")]
    TooLarge { limit: u64 },
    #[error("archive validation failed: {0}")]
    InvalidArchive(String),
    #[error("HTTP request failed while {context}; URL and response content redacted")]
    Http { context: &'static str },
    #[error("Foundry endpoint returned {status}: {context}")]
    Remote { status: StatusCode, context: String },
    #[error("Foundry account login response did not indicate success ({status})")]
    AuthenticationRejected { status: StatusCode },
    #[error("required login form field was not found: {0}")]
    MissingFormField(&'static str),
    #[error("cache metadata is invalid: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("no usable release acquisition source was supplied")]
    NoAcquisitionSource,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Foundry's channel-independent major/build release identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseId {
    pub major: u16,
    pub build: u32,
}

impl ReleaseId {
    pub fn parse(value: &str) -> Result<Self, FetchError> {
        let value = value.trim();
        let value = value
            .strip_prefix("FoundryVTT-Linux-")
            .or_else(|| value.strip_prefix("FoundryVTT-"))
            .unwrap_or(value)
            .strip_suffix(".zip")
            .unwrap_or(value);
        let normalized = if let Some((version, build)) = value.split_once('+') {
            let major = version
                .split('.')
                .next()
                .ok_or_else(|| FetchError::InvalidRelease(value.to_string()))?;
            format!("{major}.{build}")
        } else {
            value.to_owned()
        };
        let (major, build) = normalized
            .split_once('.')
            .ok_or_else(|| FetchError::InvalidRelease(value.to_string()))?;
        if major.is_empty() || build.is_empty() || build.contains('.') {
            return Err(FetchError::InvalidRelease(value.to_string()));
        }
        let major = major
            .parse()
            .map_err(|_| FetchError::InvalidRelease(value.to_string()))?;
        let build = build
            .parse()
            .map_err(|_| FetchError::InvalidRelease(value.to_string()))?;
        Ok(Self { major, build })
    }

    pub fn archive_name(&self) -> String {
        format!("FoundryVTT-Linux-{}.{}.zip", self.major, self.build)
    }
}

impl std::fmt::Display for ReleaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.build)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    #[default]
    Node,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    LicensedAccount,
    PreAcquiredArchive,
    SignedUrl,
}

/// Credentials are zeroized when dropped and are never included in `Debug`.
#[derive(Clone)]
pub struct AccountCredentials {
    pub username: String,
    password: Zeroizing<String>,
}

impl std::fmt::Debug for AccountCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountCredentials")
            .field("username", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

impl AccountCredentials {
    pub fn from_values(
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<Self, FetchError> {
        let username = username.into().trim().to_owned();
        let password = Zeroizing::new(password.into().trim_end().to_owned());
        if username.is_empty() || password.is_empty() {
            return Err(FetchError::InvalidRelease(
                "credentials must not be empty".into(),
            ));
        }
        Ok(Self { username, password })
    }

    pub fn from_files(
        username: impl AsRef<Path>,
        password: impl AsRef<Path>,
    ) -> Result<Self, FetchError> {
        let username = fs::read_to_string(username)?.trim().to_owned();
        let password = Zeroizing::new(fs::read_to_string(password)?.trim_end().to_owned());
        if username.is_empty() || password.is_empty() {
            return Err(FetchError::InvalidRelease(
                "credentials files must not be empty".into(),
            ));
        }
        Ok(Self { username, password })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub release: ReleaseId,
    pub platform: Platform,
    pub source: SourceKind,
    pub sha256: String,
    #[serde(default)]
    pub nix_sri: String,
    pub size: u64,
    #[serde(default)]
    pub acquired_at: String,
    #[serde(default)]
    pub acquired_at_unix: u64,
}

#[derive(Debug, Clone)]
pub struct Artifact {
    pub archive: PathBuf,
    pub provenance: Provenance,
}

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub site: Url,
    pub platform: Platform,
    pub max_bytes: u64,
    pub timeout: Duration,
    pub max_response_bytes: u64,
    pub retry: RetryPolicy,
    /// Test/operator supplied acquisition time. Production uses the system clock.
    pub acquired_at_unix: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u8,
    pub initial_backoff: Duration,
}

impl RetryPolicy {
    fn delay(self, retry_index: u8) -> Duration {
        self.initial_backoff
            .checked_mul(
                1_u32
                    .checked_shl(u32::from(retry_index))
                    .unwrap_or(u32::MAX),
            )
            .unwrap_or(Duration::MAX)
    }
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            site: Url::parse(DEFAULT_SITE)
                .unwrap_or_else(|_| unreachable!("default Foundry URL is valid")),
            platform: Platform::Node,
            max_bytes: DEFAULT_MAX_BYTES,
            timeout: Duration::from_secs(90),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            retry: RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(250),
            },
            acquired_at_unix: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AcquireSources {
    pub release_url_file: Option<PathBuf>,
    pub username_file: Option<PathBuf>,
    pub password_file: Option<PathBuf>,
    pub offline: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseResponse {
    url: String,
    #[serde(rename = "lifetime")]
    _lifetime: Option<u64>,
}

/// Private on-disk cache.  The archive and sidecar are atomically replaced,
/// and the cache directory is created with mode 0700.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, FetchError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        set_mode(&root, 0o700)?;
        Ok(Self { root })
    }

    /// Make an already populated cache readable by the Nix build group while
    /// retaining a non-world-readable directory and files.
    pub fn prepare_shared_read_cache(&self) -> Result<(), FetchError> {
        set_mode(&self.root, 0o2750)?;
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.is_file() {
                set_mode(&path, 0o440)?;
            }
        }
        sync_directory(&self.root)?;
        Ok(())
    }

    pub fn path_for(&self, release: &ReleaseId, platform: Platform) -> PathBuf {
        let name = match platform {
            Platform::Node => format!("FoundryVTT-Node-{release}.zip"),
            Platform::Linux => release.archive_name(),
        };
        self.root.join(name)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn metadata_path(&self, release: &ReleaseId, platform: Platform) -> PathBuf {
        self.root.join(format!(
            "foundryvtt-{}-{}.json",
            release,
            platform_name(platform)
        ))
    }

    pub fn get(
        &self,
        release: &ReleaseId,
        platform: Platform,
    ) -> Result<Option<Artifact>, FetchError> {
        self.get_with_max(release, platform, DEFAULT_MAX_BYTES)
    }

    pub fn get_with_max(
        &self,
        release: &ReleaseId,
        platform: Platform,
        max_bytes: u64,
    ) -> Result<Option<Artifact>, FetchError> {
        let archive = self.path_for(release, platform);
        let metadata = self.metadata_path(release, platform);
        if !archive.is_file() || !metadata.is_file() {
            return Ok(None);
        }
        let mut provenance: Provenance = serde_json::from_slice(&fs::read(&metadata)?)?;
        if provenance.release != *release || provenance.platform != platform {
            return Err(FetchError::InvalidArchive(
                "cache provenance does not match requested release".into(),
            ));
        }
        let (sha256, size) = validate_archive(&archive, release, max_bytes)?;
        if !provenance.sha256.is_empty() && provenance.sha256 != sha256 {
            return Err(FetchError::InvalidArchive(
                "cache checksum does not match metadata".into(),
            ));
        }
        if provenance.size != 0 && provenance.size != size {
            return Err(FetchError::InvalidArchive(
                "cache size does not match metadata".into(),
            ));
        }
        if provenance.sha256.is_empty() {
            provenance.sha256 = sha256.clone();
        }
        if provenance.nix_sri.is_empty() {
            provenance.nix_sri = nix_sri_from_hex(&sha256)?;
        }
        if provenance.size == 0 {
            provenance.size = size;
        }
        Ok(Some(Artifact {
            archive,
            provenance,
        }))
    }

    pub fn put(
        &self,
        release: &ReleaseId,
        platform: Platform,
        source: SourceKind,
        input: &Path,
        max_bytes: u64,
    ) -> Result<Artifact, FetchError> {
        self.put_at(release, platform, source, input, max_bytes, now_unix())
    }

    fn put_at(
        &self,
        release: &ReleaseId,
        platform: Platform,
        source: SourceKind,
        input: &Path,
        max_bytes: u64,
        acquired_at_unix: u64,
    ) -> Result<Artifact, FetchError> {
        let (sha256, size) = validate_archive(input, release, max_bytes)?;
        let archive = self.path_for(release, platform);
        let metadata = self.metadata_path(release, platform);
        let mut tmp = NamedTempFile::new_in(&self.root)?;
        copy_file(input, tmp.as_file_mut(), max_bytes)?;
        tmp.as_file().sync_all()?;
        let tmp_path = tmp.into_temp_path();
        fs::rename(&tmp_path, &archive)?;
        let provenance = Provenance {
            release: release.clone(),
            platform,
            source,
            sha256: sha256.clone(),
            nix_sri: nix_sri_from_hex(&sha256)?,
            size,
            acquired_at: OffsetDateTime::from_unix_timestamp(acquired_at_unix as i64)
                .ok()
                .and_then(|time| time.format(&Rfc3339).ok())
                .unwrap_or_else(|| acquired_at_unix.to_string()),
            acquired_at_unix,
        };
        let json = serde_json::to_vec_pretty(&provenance)?;
        let mut meta_tmp = NamedTempFile::new_in(&self.root)?;
        meta_tmp.write_all(&json)?;
        meta_tmp.as_file().sync_all()?;
        let meta_tmp_path = meta_tmp.into_temp_path();
        fs::rename(&meta_tmp_path, &metadata)?;
        set_mode(&archive, 0o600)?;
        set_mode(&metadata, 0o600)?;
        sync_directory(&self.root)?;
        Ok(Artifact {
            archive,
            provenance,
        })
    }
}

/// Validate and hash a pre-acquired archive.  The ZIP is checked for a
/// package.json, a matching release, path traversal, and unsafe symlink targets.
pub fn validate_archive(
    path: &Path,
    release: &ReleaseId,
    max_bytes: u64,
) -> Result<(String, u64), FetchError> {
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(FetchError::NotRegularFile(path.to_path_buf()));
    }
    if meta.len() > max_bytes {
        return Err(FetchError::TooLarge { limit: max_bytes });
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut zip = ZipArchive::new(file).map_err(|e| FetchError::InvalidArchive(e.to_string()))?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(FetchError::InvalidArchive(format!(
            "ZIP contains more than {MAX_ZIP_ENTRIES} entries"
        )));
    }
    let mut package_json = None;
    let mut expanded = 0_u64;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| FetchError::InvalidArchive(e.to_string()))?;
        let name = entry.name().replace('\\', "/");
        if !safe_zip_path(&name) {
            return Err(FetchError::InvalidArchive(format!(
                "unsafe ZIP path {name:?}"
            )));
        }
        if is_symlink(&entry) {
            let mut target = String::new();
            entry.read_to_string(&mut target).map_err(|_| {
                FetchError::InvalidArchive(format!("symlink target for {name:?} is not UTF-8"))
            })?;
            if !safe_symlink_target(&name, &target) {
                return Err(FetchError::InvalidArchive(format!(
                    "unsafe symlink target for {name:?}"
                )));
            }
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or_else(|| FetchError::InvalidArchive("ZIP expansion size overflow".into()))?;
        if expanded > max_bytes {
            return Err(FetchError::TooLarge { limit: max_bytes });
        }
        if name == "resources/app/package.json" || name == "package.json" {
            if entry.size() > MAX_PACKAGE_JSON_BYTES {
                return Err(FetchError::InvalidArchive(
                    "package.json is too large".into(),
                ));
            }
            let mut bytes = Vec::new();
            let mut reader = entry;
            reader.read_to_end(&mut bytes)?;
            package_json = Some(bytes);
        }
    }
    let package_json =
        package_json.ok_or_else(|| FetchError::InvalidArchive("missing package.json".into()))?;
    let package: serde_json::Value = serde_json::from_slice(&package_json)
        .map_err(|e| FetchError::InvalidArchive(format!("invalid package.json: {e}")))?;
    validate_package_release(&package, release)?;
    Ok((hex::encode(hasher.finalize()), meta.len()))
}

fn validate_package_release(
    package: &serde_json::Value,
    release: &ReleaseId,
) -> Result<(), FetchError> {
    if let Some(release_obj) = package
        .get("release")
        .and_then(serde_json::Value::as_object)
    {
        let generation = json_u32(release_obj.get("generation")).ok_or_else(|| {
            FetchError::InvalidArchive("package.json release.generation is missing".into())
        })?;
        let build = json_u32(release_obj.get("build")).ok_or_else(|| {
            FetchError::InvalidArchive("package.json release.build is missing".into())
        })?;
        if generation != u32::from(release.major) || build != release.build {
            return Err(FetchError::InvalidArchive(format!(
                "package.json release {generation}.{build} does not match requested {release}"
            )));
        }
        return Ok(());
    }
    if release.major >= 13 {
        return Err(FetchError::InvalidArchive(
            "modern Foundry releases require package.json release.generation and release.build"
                .into(),
        ));
    }
    if let Some(version) = package.get("version").and_then(serde_json::Value::as_str) {
        let found = ReleaseId::parse(version).map_err(|_| {
            FetchError::InvalidArchive("package.json has an invalid version".into())
        })?;
        if found != *release {
            return Err(FetchError::InvalidArchive(format!(
                "package.json release {found} does not match requested {release}"
            )));
        }
        return Ok(());
    }
    Err(FetchError::InvalidArchive(
        "package.json has no release/version field".into(),
    ))
}

fn json_u32(value: Option<&serde_json::Value>) -> Option<u32> {
    value.and_then(|value| {
        value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .or_else(|| value.as_str()?.parse().ok())
    })
}

/// Acquire an archive through the licensed account flow documented by
/// Foundry's web release endpoint.  Signed URLs remain in memory only.
pub async fn acquire_account(
    cache: &Cache,
    release: &ReleaseId,
    credentials: &AccountCredentials,
    options: &FetchOptions,
) -> Result<Artifact, FetchError> {
    let client = foundry_client(options)?;
    let login_url = join_url(&options.site, "auth/login/")?;
    let login = send_with_retry(
        client.get(login_url.clone()),
        options,
        "fetching login page",
    )
    .await?;
    let login_text = checked_text(login, "login page", options.max_response_bytes).await?;
    let csrf = extract_input(&login_text, "csrfmiddlewaretoken")
        .ok_or(FetchError::MissingFormField("csrfmiddlewaretoken"))?;
    let mut form = std::collections::HashMap::new();
    form.insert("username", credentials.username.as_str());
    form.insert("password", credentials.password.as_str());
    form.insert("csrfmiddlewaretoken", csrf.as_str());
    let response = send_with_retry(
        client
            .post(login_url.clone())
            // Foundry enforces Django's HTTPS CSRF referer check in addition
            // to the cookie and hidden form token.
            .header(reqwest::header::REFERER, login_url.as_str())
            .form(&form),
        options,
        "submitting account login",
    )
    .await?;
    let response_status = response.status();
    let response_text = checked_text(response, "account login", options.max_response_bytes).await?;
    if !response_text.contains("login-welcome") && !response_text.contains("logout") {
        return Err(FetchError::AuthenticationRejected {
            status: response_status,
        });
    }
    let mut endpoint = join_url(&options.site, "releases/download")?;
    endpoint
        .query_pairs_mut()
        .append_pair("build", &release.build.to_string())
        .append_pair("platform", platform_name(options.platform))
        .append_pair("response_type", "json");
    let release_response =
        send_with_retry(client.get(endpoint), options, "requesting release URL").await?;
    let release_response = checked_json::<ReleaseResponse>(
        release_response,
        "release URL endpoint",
        options.max_response_bytes,
    )
    .await?;
    let signed_url = Url::parse(&release_response.url).map_err(|_| {
        FetchError::InvalidUrl("release endpoint returned an invalid download URL".into())
    })?;
    validate_download_url(&signed_url, &options.site)?;
    download_to_cache(
        cache,
        release,
        signed_url,
        options,
        SourceKind::LicensedAccount,
    )
    .await
}

pub async fn acquire_signed_url(
    cache: &Cache,
    release: &ReleaseId,
    url_file: &Path,
    options: &FetchOptions,
) -> Result<Artifact, FetchError> {
    let url = fs::read_to_string(url_file)?.trim().to_owned();
    if url.is_empty() {
        return Err(FetchError::InvalidUrl("signed URL file is empty".into()));
    }
    let url = Url::parse(&url)
        .map_err(|_| FetchError::InvalidUrl("signed URL file contains an invalid URL".into()))?;
    if url.scheme() != "https" {
        return Err(FetchError::InvalidUrl("signed URL must use HTTPS".into()));
    }
    download_to_cache(cache, release, url, options, SourceKind::SignedUrl).await
}

pub async fn acquire(
    cache: &Cache,
    release: &ReleaseId,
    sources: &AcquireSources,
    options: &FetchOptions,
) -> Result<Artifact, FetchError> {
    let cached = || cache.get_with_max(release, options.platform, options.max_bytes);
    let mut last_error = None;
    if !sources.offline {
        if let Some(path) = &sources.release_url_file {
            match acquire_signed_url(cache, release, path, options).await {
                Ok(artifact) => return Ok(artifact),
                Err(error) => last_error = Some(error),
            }
        }
        match (&sources.username_file, &sources.password_file) {
            (Some(username), Some(password)) => {
                let credentials = AccountCredentials::from_files(username, password)?;
                match acquire_account(cache, release, &credentials, options).await {
                    Ok(artifact) => return Ok(artifact),
                    Err(error) => last_error = Some(error),
                }
            }
            (Some(_), None) | (None, Some(_)) => return Err(FetchError::NoAcquisitionSource),
            (None, None) => {}
        }
    }
    cached()?.ok_or_else(|| last_error.unwrap_or(FetchError::NoAcquisitionSource))
}

/// Cache a caller-provided licensed archive after validation.
pub fn acquire_archive(
    cache: &Cache,
    release: &ReleaseId,
    archive: &Path,
    platform: Platform,
    max_bytes: u64,
) -> Result<Artifact, FetchError> {
    cache.put(
        release,
        platform,
        SourceKind::PreAcquiredArchive,
        archive,
        max_bytes,
    )
}

async fn download_to_cache(
    cache: &Cache,
    release: &ReleaseId,
    url: Url,
    options: &FetchOptions,
    source: SourceKind,
) -> Result<Artifact, FetchError> {
    validate_download_url(&url, &options.site)?;
    let client = download_client(options)?;
    let response = send_with_retry(client.get(url), options, "downloading Foundry archive").await?;
    if !response.status().is_success() {
        return Err(FetchError::Remote {
            status: response.status(),
            context: "Foundry archive download failed".into(),
        });
    }
    let mut tmp = NamedTempFile::new_in(cache.root())?;
    let mut size = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| FetchError::Http {
            context: "streaming Foundry archive",
        })?;
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or(FetchError::TooLarge {
                limit: options.max_bytes,
            })?;
        if size > options.max_bytes {
            return Err(FetchError::TooLarge {
                limit: options.max_bytes,
            });
        }
        tmp.write_all(&chunk)?;
    }
    tmp.as_file().sync_all()?;
    cache.put_at(
        release,
        options.platform,
        source,
        tmp.path(),
        options.max_bytes,
        options.acquired_at_unix.unwrap_or_else(now_unix),
    )
}

fn foundry_client(options: &FetchOptions) -> Result<Client, FetchError> {
    let expected_origin = origin(&options.site);
    Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 || origin(attempt.url()) != expected_origin {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(Duration::from_secs(15))
        .timeout(options.timeout)
        .user_agent("foundryvtt-fetch/0.1")
        .build()
        .map_err(|_| FetchError::Http {
            context: "building Foundry HTTP client",
        })
}

fn download_client(options: &FetchOptions) -> Result<Client, FetchError> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !secure_or_loopback(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(Duration::from_secs(15))
        .timeout(options.timeout)
        .user_agent("foundryvtt-fetch/0.1")
        .build()
        .map_err(|_| FetchError::Http {
            context: "building download HTTP client",
        })
}

fn join_url(site: &Url, path: &str) -> Result<Url, FetchError> {
    site.join(path)
        .map_err(|_| FetchError::InvalidUrl(format!("cannot join {path:?} to Foundry site")))
}

async fn checked_text(
    response: Response,
    context: &str,
    max_bytes: u64,
) -> Result<String, FetchError> {
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Remote {
            status,
            context: context.into(),
        });
    }
    let body = read_limited_body(response, max_bytes, "reading HTTP response").await?;
    String::from_utf8(body)
        .map_err(|_| FetchError::InvalidArchive(format!("{context} was not valid UTF-8")))
}

async fn checked_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    context: &str,
    max_bytes: u64,
) -> Result<T, FetchError> {
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Remote {
            status,
            context: context.into(),
        });
    }
    let body = read_limited_body(response, max_bytes, "reading JSON response").await?;
    serde_json::from_slice(&body).map_err(FetchError::Metadata)
}

async fn send_with_retry(
    request: RequestBuilder,
    options: &FetchOptions,
    context: &'static str,
) -> Result<Response, FetchError> {
    let attempts = options.retry.max_attempts.max(1);
    for attempt in 0..attempts {
        let next = request.try_clone().ok_or(FetchError::Http { context })?;
        match next.send().await {
            Ok(response) if retryable_status(response.status()) && attempt + 1 < attempts => {
                tokio::time::sleep(options.retry.delay(attempt)).await;
            }
            Ok(response) => return Ok(response),
            Err(_) if attempt + 1 < attempts => {
                tokio::time::sleep(options.retry.delay(attempt)).await;
            }
            Err(_) => return Err(FetchError::Http { context }),
        }
    }
    Err(FetchError::Http { context })
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

async fn read_limited_body(
    response: Response,
    max_bytes: u64,
    context: &'static str,
) -> Result<Vec<u8>, FetchError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| FetchError::Http { context })?;
        let next = u64::try_from(body.len())
            .ok()
            .and_then(|size| size.checked_add(chunk.len() as u64))
            .ok_or(FetchError::TooLarge { limit: max_bytes })?;
        if next > max_bytes {
            return Err(FetchError::TooLarge { limit: max_bytes });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn origin(url: &Url) -> (String, Option<String>, Option<u16>) {
    (
        url.scheme().to_owned(),
        url.host_str().map(str::to_owned),
        url.port_or_known_default(),
    )
}

fn secure_or_loopback(url: &Url) -> bool {
    url.scheme() == "https"
        || url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

fn validate_download_url(url: &Url, site: &Url) -> Result<(), FetchError> {
    if secure_or_loopback(url) && (url.scheme() == "https" || secure_or_loopback(site)) {
        Ok(())
    } else {
        Err(FetchError::InvalidUrl(
            "download URL must use HTTPS (loopback HTTP is allowed only for tests)".into(),
        ))
    }
}

fn extract_input(html: &str, name: &'static str) -> Option<String> {
    html.split('<')
        .filter_map(|fragment| fragment.strip_prefix("input"))
        .find_map(|attributes| {
            let input_name = html_attribute(attributes, "name")?;
            if input_name != name {
                return None;
            }
            html_attribute(attributes, "value")
        })
}

fn html_attribute(fragment: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=");
    let start = fragment.find(&marker)? + marker.len();
    let rest = fragment[start..].trim_start();
    let quote = rest.as_bytes().first().copied()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let end = rest[1..].find(char::from(quote))? + 1;
    Some(rest[1..end].to_owned())
}

fn safe_zip_path(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.contains(':') {
        return false;
    }
    Path::new(name)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn is_symlink(entry: &zip::read::ZipFile<'_>) -> bool {
    entry
        .unix_mode()
        .is_some_and(|mode| mode & 0o170000 == 0o120000)
}

fn safe_symlink_target(name: &str, target: &str) -> bool {
    let target = target.trim_end_matches('\0').replace('\\', "/");
    if target.is_empty() || target.starts_with('/') || target.contains(':') {
        return false;
    }
    let mut resolved = Path::new(name)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    for component in Path::new(&target).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir if resolved.pop() => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

fn copy_file(input: &Path, output: &mut File, max_bytes: u64) -> Result<(), FetchError> {
    let mut source = File::open(input)?;
    let mut copied = 0_u64;
    let mut buf = [0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buf)?;
        if read == 0 {
            break;
        }
        copied = copied.saturating_add(read as u64);
        if copied > max_bytes {
            return Err(FetchError::TooLarge { limit: max_bytes });
        }
        output.write_all(&buf[..read])?;
    }
    Ok(())
}

fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Node => "node",
        Platform::Linux => "linux",
    }
}

fn nix_sri_from_hex(hex_digest: &str) -> Result<String, FetchError> {
    let digest = hex::decode(hex_digest)
        .map_err(|_| FetchError::InvalidArchive("invalid SHA-256 digest".into()))?;
    if digest.len() != 32 {
        return Err(FetchError::InvalidArchive(
            "SHA-256 digest has the wrong length".into(),
        ));
    }
    Ok(format!("sha256-{}", BASE64_STANDARD.encode(digest)))
}

fn sync_directory(path: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::{Form, Query, State},
        http::{HeaderMap, StatusCode as HttpStatus, header},
        response::{IntoResponse, Json},
        routing::get,
    };
    use std::io::Write;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use zip::write::SimpleFileOptions;

    fn archive(path: &Path, package: serde_json::Value, platform: Platform) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let prefix = if platform == Platform::Linux {
            "resources/app/"
        } else {
            ""
        };
        writer
            .start_file(
                format!("{prefix}package.json"),
                SimpleFileOptions::default(),
            )
            .unwrap();
        writer
            .write_all(serde_json::to_string(&package).unwrap().as_bytes())
            .unwrap();
        writer
            .start_file(format!("{prefix}index.js"), SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"ok").unwrap();
        writer.finish().unwrap();
    }

    fn modern_package(major: u16, build: u32) -> serde_json::Value {
        serde_json::json!({
            "version": format!("{major}.0.0+{build}"),
            "release": {"generation": major, "build": build}
        })
    }

    #[test]
    fn parses_release_names() {
        assert_eq!(
            ReleaseId::parse("FoundryVTT-Linux-13.351.zip").unwrap(),
            ReleaseId {
                major: 13,
                build: 351
            }
        );
        assert_eq!(
            ReleaseId::parse("13.351").unwrap().archive_name(),
            "FoundryVTT-Linux-13.351.zip"
        );
        assert_eq!(
            ReleaseId::parse("14.0.0+412").unwrap(),
            ReleaseId {
                major: 14,
                build: 412
            }
        );
        assert!(ReleaseId::parse("13").is_err());
    }

    #[test]
    fn validates_and_hashes_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release.zip");
        archive(&path, modern_package(13, 351), Platform::Linux);
        let result = validate_archive(
            &path,
            &ReleaseId {
                major: 13,
                build: 351,
            },
            1024 * 1024,
        )
        .unwrap();
        assert_eq!(result.1, fs::metadata(path).unwrap().len());
        assert_eq!(result.0.len(), 64);
    }

    #[test]
    fn validates_node_layout_and_rejects_modern_legacy_bypass() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("node.zip");
        archive(&node, modern_package(14, 412), Platform::Node);
        validate_archive(
            &node,
            &ReleaseId {
                major: 14,
                build: 412,
            },
            1024 * 1024,
        )
        .unwrap();

        let conflict = dir.path().join("conflict.zip");
        archive(
            &conflict,
            serde_json::json!({
                "version": "13.0.0+351",
                "release": {"generation": 14, "build": 412}
            }),
            Platform::Linux,
        );
        assert!(matches!(
            validate_archive(&conflict, &ReleaseId { major: 13, build: 351 }, 1024 * 1024),
            Err(FetchError::InvalidArchive(message)) if message.contains("does not match")
        ));

        let legacy_only = dir.path().join("legacy-only.zip");
        archive(
            &legacy_only,
            serde_json::json!({"version": "13.0.0+351"}),
            Platform::Linux,
        );
        assert!(matches!(
            validate_archive(&legacy_only, &ReleaseId { major: 13, build: 351 }, 1024 * 1024),
            Err(FetchError::InvalidArchive(message)) if message.contains("modern Foundry")
        ));
    }

    #[test]
    fn rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release.zip");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("../escape", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
        assert!(matches!(
            validate_archive(
                &path,
                &ReleaseId {
                    major: 13,
                    build: 351
                },
                1024
            ),
            Err(FetchError::InvalidArchive(_))
        ));
    }

    #[test]
    fn accepts_safe_symlink_targets_and_rejects_escape() {
        assert!(safe_symlink_target(
            "resources/app/node_modules/.bin/crc32",
            "../crc32/bin/crc32.js"
        ));
        assert!(!safe_symlink_target(
            "resources/app/node_modules/.bin/crc32",
            "../../../../../../etc/shadow"
        ));
        assert!(!safe_symlink_target(
            "resources/app/node_modules/.bin/crc32",
            "/etc/shadow"
        ));
    }

    #[derive(Clone)]
    struct FakeState {
        archive: Arc<Vec<u8>>,
        base: Arc<String>,
        requested_build: Arc<Mutex<Option<String>>>,
        release_attempts: Arc<AtomicUsize>,
        login_valid: Arc<Mutex<bool>>,
    }

    async fn fake_login() -> &'static str {
        "<input name=\"csrfmiddlewaretoken\" value=\"token\">"
    }
    async fn fake_post_login(
        State(state): State<FakeState>,
        headers: HeaderMap,
        Form(form): Form<std::collections::HashMap<String, String>>,
    ) -> axum::response::Response {
        let expected_referer = format!("{}/auth/login/", state.base);
        let valid = form.get("csrfmiddlewaretoken").map(String::as_str) == Some("token")
            && form.get("username").map(String::as_str) == Some("user@example.test")
            && form.get("password").map(String::as_str) == Some("secret")
            && headers
                .get(header::REFERER)
                .and_then(|value| value.to_str().ok())
                == Some(expected_referer.as_str());
        *state.login_valid.lock().unwrap() = valid;
        if valid {
            (
                HttpStatus::OK,
                [(header::SET_COOKIE, "sessionid=test; Path=/; HttpOnly")],
                "login-welcome",
            )
                .into_response()
        } else {
            (HttpStatus::UNAUTHORIZED, "rejected").into_response()
        }
    }
    async fn fake_release(
        State(state): State<FakeState>,
        Query(query): Query<std::collections::HashMap<String, String>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        if headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            != Some("sessionid=test")
        {
            return (
                HttpStatus::UNAUTHORIZED,
                Json(serde_json::json!({"error": "cookie"})),
            );
        }
        *state.requested_build.lock().unwrap() = query.get("build").cloned();
        if state.release_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return (
                HttpStatus::TOO_MANY_REQUESTS,
                Json(serde_json::json!({"error": "retry"})),
            );
        }
        assert_eq!(query.get("platform").map(String::as_str), Some("linux"));
        assert_eq!(query.get("response_type").map(String::as_str), Some("json"));
        (
            HttpStatus::OK,
            Json(serde_json::json!({"url": format!("{}/archive.zip", state.base), "lifetime": 60})),
        )
    }
    async fn fake_archive(State(state): State<FakeState>) -> impl IntoResponse {
        (HttpStatus::OK, Body::from((*state.archive).clone()))
    }

    #[test]
    fn account_flow_uses_numeric_build_and_validates_archive() {
        std::thread::Builder::new()
            .name("foundry-fetch-fake-server".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(account_flow_uses_numeric_build_and_validates_archive_inner());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    async fn account_flow_uses_numeric_build_and_validates_archive_inner() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("source.zip");
        archive(&archive_path, modern_package(13, 351), Platform::Linux);
        let bytes = Arc::new(fs::read(&archive_path).unwrap());
        let requested_build = Arc::new(Mutex::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Arc::new(format!("http://{}", listener.local_addr().unwrap()));
        let login_valid = Arc::new(Mutex::new(false));
        let release_attempts = Arc::new(AtomicUsize::new(0));
        let state = FakeState {
            archive: bytes,
            base: base.clone(),
            requested_build: requested_build.clone(),
            release_attempts: release_attempts.clone(),
            login_valid: login_valid.clone(),
        };
        let app = Router::new()
            .route("/auth/login/", get(fake_login).post(fake_post_login))
            .route("/releases/download", get(fake_release))
            .route("/archive.zip", get(fake_archive))
            .with_state(state);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cache = Cache::new(temp.path().join("cache")).unwrap();
        let credentials = AccountCredentials {
            username: "user@example.test".into(),
            password: Zeroizing::new("secret".into()),
        };
        let options = FetchOptions {
            site: Url::parse(&base).unwrap(),
            platform: Platform::Linux,
            retry: RetryPolicy {
                max_attempts: 2,
                initial_backoff: Duration::ZERO,
            },
            acquired_at_unix: Some(1_700_000_000),
            ..FetchOptions::default()
        };
        let artifact = acquire_account(
            &cache,
            &ReleaseId {
                major: 13,
                build: 351,
            },
            &credentials,
            &options,
        )
        .await
        .unwrap();
        assert_eq!(artifact.provenance.source, SourceKind::LicensedAccount);
        assert_eq!(requested_build.lock().unwrap().as_deref(), Some("351"));
        assert_eq!(artifact.provenance.acquired_at_unix, 1_700_000_000);
        assert!(*login_valid.lock().unwrap());
        assert_eq!(release_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            artifact.archive.file_name().unwrap(),
            "FoundryVTT-Linux-13.351.zip"
        );
    }

    #[test]
    fn exact_offline_cache_fallback_is_verified() {
        std::thread::Builder::new()
            .name("foundry-fetch-offline-cache".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let temp = tempfile::tempdir().unwrap();
                let source = temp.path().join("source.zip");
                archive(&source, modern_package(13, 351), Platform::Node);
                let cache = Cache::new(temp.path().join("cache")).unwrap();
                let release = ReleaseId {
                    major: 13,
                    build: 351,
                };
                cache
                    .put_at(
                        &release,
                        Platform::Node,
                        SourceKind::PreAcquiredArchive,
                        &source,
                        1024 * 1024,
                        42,
                    )
                    .unwrap();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let artifact = runtime
                    .block_on(acquire(
                        &cache,
                        &release,
                        &AcquireSources {
                            offline: true,
                            ..AcquireSources::default()
                        },
                        &FetchOptions {
                            platform: Platform::Node,
                            ..FetchOptions::default()
                        },
                    ))
                    .unwrap();
                assert_eq!(artifact.provenance.acquired_at_unix, 42);
                assert!(
                    cache
                        .get(
                            &ReleaseId {
                                major: 13,
                                build: 352
                            },
                            Platform::Node
                        )
                        .unwrap()
                        .is_none()
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn credentials_and_http_errors_are_redacted() {
        let credentials = AccountCredentials {
            username: "private@example.test".into(),
            password: Zeroizing::new("super-secret".into()),
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("private@example.test"));
        assert!(!debug.contains("super-secret"));
        let error = FetchError::Http {
            context: "downloading Foundry archive",
        }
        .to_string();
        assert!(!error.contains("http://"));
        assert!(!error.contains("https://"));
        let rejected_login = FetchError::AuthenticationRejected {
            status: StatusCode::OK,
        }
        .to_string();
        assert!(rejected_login.contains("200 OK"));
    }

    #[test]
    fn retry_backoff_is_bounded_and_deterministic() {
        let retry = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(125),
        };
        assert_eq!(retry.delay(0), Duration::from_millis(125));
        assert_eq!(retry.delay(1), Duration::from_millis(250));
        assert_eq!(retry.delay(2), Duration::from_millis(500));
    }

    #[test]
    fn rejected_login_fails_closed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = Arc::new(format!("http://{}", listener.local_addr().unwrap()));
            let state = FakeState {
                archive: Arc::new(Vec::new()),
                base: base.clone(),
                requested_build: Arc::new(Mutex::new(None)),
                release_attempts: Arc::new(AtomicUsize::new(0)),
                login_valid: Arc::new(Mutex::new(false)),
            };
            let app = Router::new()
                .route("/auth/login/", get(fake_login).post(fake_post_login))
                .with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let cache = Cache::new(temp.path().join("cache")).unwrap();
            let credentials = AccountCredentials {
                username: "user@example.test".into(),
                password: Zeroizing::new("wrong".into()),
            };
            let options = FetchOptions {
                site: Url::parse(&base).unwrap(),
                retry: RetryPolicy {
                    max_attempts: 1,
                    initial_backoff: Duration::ZERO,
                },
                ..FetchOptions::default()
            };
            assert!(matches!(
                acquire_account(
                    &cache,
                    &ReleaseId {
                        major: 13,
                        build: 351
                    },
                    &credentials,
                    &options,
                )
                .await,
                Err(FetchError::Remote {
                    status: StatusCode::UNAUTHORIZED,
                    ..
                })
            ));
        });
    }
}
