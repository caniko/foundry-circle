//! Credential-safe Foundry VTT release acquisition.
//!
//! This crate intentionally owns only acquisition and validation.  Nix update
//! tooling consumes [`Artifact::provenance`] and writes exact hashes to the
//! nix-foundryvtt lock; Nix evaluation/builds never contact foundryvtt.com.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url};
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
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("remote release endpoint returned {status}: {context}")]
    Remote { status: StatusCode, context: String },
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
        let (major, build) = value
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
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl AccountCredentials {
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
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            site: Url::parse(DEFAULT_SITE)
                .unwrap_or_else(|_| unreachable!("default Foundry URL is valid")),
            platform: Platform::Node,
            max_bytes: DEFAULT_MAX_BYTES,
            timeout: Duration::from_secs(90),
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

    pub fn path_for(&self, release: &ReleaseId, platform: Platform) -> PathBuf {
        self.root.join(format!(
            "foundryvtt-{}-{}.zip",
            release,
            platform_name(platform)
        ))
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
        let (sha256, size) = validate_archive(input, release, max_bytes)?;
        let archive = self.path_for(release, platform);
        let metadata = self.metadata_path(release, platform);
        let mut tmp = NamedTempFile::new_in(&self.root)?;
        copy_file(input, tmp.as_file_mut(), max_bytes)?;
        tmp.as_file().sync_all()?;
        let tmp_path = tmp.into_temp_path();
        fs::rename(&tmp_path, &archive)?;
        let acquired_at_unix = now_unix();
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
/// package.json, a matching release, path traversal, and symlink entries.
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
        let entry = zip
            .by_index(index)
            .map_err(|e| FetchError::InvalidArchive(e.to_string()))?;
        let name = entry.name().replace('\\', "/");
        if !safe_zip_path(&name) {
            return Err(FetchError::InvalidArchive(format!(
                "unsafe ZIP path {name:?}"
            )));
        }
        if is_symlink(&entry) {
            return Err(FetchError::InvalidArchive(format!(
                "symlink ZIP entry {name:?}"
            )));
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
    let release_obj = package
        .get("release")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            FetchError::InvalidArchive("package.json has no release/version field".into())
        })?;
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
    Ok(())
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
    let client = http_client(options)?;
    let login_url = join_url(&options.site, "auth/login/")?;
    let login = client.get(login_url.clone()).send().await?;
    let login_text = checked_text(login, "login page").await?;
    let csrf = extract_input(&login_text, "csrfmiddlewaretoken")
        .ok_or(FetchError::MissingFormField("csrfmiddlewaretoken"))?;
    let mut form = std::collections::HashMap::new();
    form.insert("username", credentials.username.as_str());
    form.insert("password", credentials.password.as_str());
    form.insert("csrfmiddlewaretoken", csrf.as_str());
    let response = client.post(login_url).form(&form).send().await?;
    let response_text = checked_text(response, "account login").await?;
    if !response_text.contains("login-welcome") && !response_text.contains("logout") {
        return Err(FetchError::Remote {
            status: StatusCode::UNAUTHORIZED,
            context: "Foundry account login was rejected".into(),
        });
    }
    let mut endpoint = join_url(&options.site, "releases/download")?;
    endpoint
        .query_pairs_mut()
        .append_pair("build", &release.build.to_string())
        .append_pair("platform", platform_name(options.platform))
        .append_pair("response_type", "json");
    let release_response = client.get(endpoint).send().await?;
    let release_response =
        checked_json::<ReleaseResponse>(release_response, "release URL endpoint").await?;
    let signed_url = Url::parse(&release_response.url).map_err(|_| {
        FetchError::InvalidUrl("release endpoint returned an invalid download URL".into())
    })?;
    let response = client.get(signed_url).send().await?;
    if !response.status().is_success() {
        return Err(FetchError::Remote {
            status: response.status(),
            context: "Foundry archive download failed".into(),
        });
    }
    let mut tmp = NamedTempFile::new_in(&cache.root)?;
    let mut size = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size = size.saturating_add(chunk.len() as u64);
        if size > options.max_bytes {
            return Err(FetchError::TooLarge {
                limit: options.max_bytes,
            });
        }
        tmp.write_all(&chunk)?;
    }
    tmp.as_file().sync_all()?;
    let staged = tmp.path().to_path_buf();
    let artifact = cache.put(
        release,
        options.platform,
        SourceKind::LicensedAccount,
        &staged,
        options.max_bytes,
    )?;
    Ok(artifact)
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
    let client = http_client(options)?;
    let response = client.get(url).send().await?;
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
        let chunk = chunk?;
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
    cache.put(
        release,
        options.platform,
        source,
        tmp.path(),
        options.max_bytes,
    )
}

fn http_client(options: &FetchOptions) -> Result<Client, FetchError> {
    Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(15))
        .timeout(options.timeout)
        .user_agent("foundryvtt-fetch/0.1")
        .build()
        .map_err(FetchError::Http)
}

fn join_url(site: &Url, path: &str) -> Result<Url, FetchError> {
    site.join(path)
        .map_err(|_| FetchError::InvalidUrl(format!("cannot join {path:?} to Foundry site")))
}

async fn checked_text(response: reqwest::Response, context: &str) -> Result<String, FetchError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        let _ = body;
        return Err(FetchError::Remote {
            status,
            context: context.into(),
        });
    }
    Ok(body)
}

async fn checked_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    context: &str,
) -> Result<T, FetchError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        let _ = body;
        return Err(FetchError::Remote {
            status,
            context: context.into(),
        });
    }
    serde_json::from_str(&body).map_err(FetchError::Metadata)
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
        extract::{Query, State},
        http::{StatusCode as HttpStatus, header},
        response::{IntoResponse, Json},
        routing::get,
    };
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use zip::write::SimpleFileOptions;

    fn archive(path: &Path, package_version: &str) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("resources/app/package.json", SimpleFileOptions::default())
            .unwrap();
        write!(writer, "{{\"version\":\"{package_version}\"}}").unwrap();
        writer
            .start_file("resources/app/index.js", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"ok").unwrap();
        writer.finish().unwrap();
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
        assert!(ReleaseId::parse("13").is_err());
    }

    #[test]
    fn validates_and_hashes_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release.zip");
        archive(&path, "13.351");
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

    #[derive(Clone)]
    struct FakeState {
        archive: Arc<Vec<u8>>,
        base: Arc<String>,
        requested_build: Arc<Mutex<Option<String>>>,
    }

    async fn fake_login() -> &'static str {
        "<input name=\"csrfmiddlewaretoken\" value=\"token\">"
    }
    async fn fake_post_login() -> impl IntoResponse {
        (
            HttpStatus::OK,
            [(header::SET_COOKIE, "sessionid=test")],
            "login-welcome",
        )
    }
    async fn fake_release(
        State(state): State<FakeState>,
        Query(query): Query<std::collections::HashMap<String, String>>,
    ) -> impl IntoResponse {
        *state.requested_build.lock().unwrap() = query.get("build").cloned();
        Json(serde_json::json!({"url": format!("{}/archive.zip", state.base), "lifetime": 60}))
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
        archive(&archive_path, "13.351");
        let bytes = Arc::new(fs::read(&archive_path).unwrap());
        let requested_build = Arc::new(Mutex::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Arc::new(format!("http://{}", listener.local_addr().unwrap()));
        let state = FakeState {
            archive: bytes,
            base: base.clone(),
            requested_build: requested_build.clone(),
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
    }
}
