//! Credential-safe Foundry VTT release acquisition.
//!
//! This crate intentionally owns only acquisition and validation.  Nix update
//! tooling consumes [`Artifact::provenance`] and writes exact hashes to the
//! nix-foundryvtt lock; Nix evaluation/builds never contact foundryvtt.com.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;
use zip::ZipArchive;

const DEFAULT_SITE: &str = "https://foundryvtt.com";
const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("invalid release identifier: {0}")]
    InvalidRelease(String),
    #[error("archive is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("archive exceeds the configured size limit ({limit} bytes)")]
    TooLarge { limit: u64 },
    #[error("archive validation failed: {0}")]
    InvalidArchive(String),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("remote release endpoint returned {status}: {body}")]
    Remote { status: StatusCode, body: String },
    #[error("required login form field was not found: {0}")]
    MissingFormField(&'static str),
    #[error("cache metadata is invalid: {0}")]
    Metadata(#[from] serde_json::Error),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Node,
}

impl Default for Platform {
    fn default() -> Self {
        Self::Node
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    LicensedAccount,
    PreAcquiredArchive,
    SignedUrl,
}

/// Credentials are kept as owned strings and are never included in `Debug`.
#[derive(Clone)]
pub struct AccountCredentials {
    pub username: String,
    password: String,
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
    pub fn from_files(username: impl AsRef<Path>, password: impl AsRef<Path>) -> Result<Self, FetchError> {
        let username = fs::read_to_string(username)?.trim().to_owned();
        let password = fs::read_to_string(password)?.trim_end().to_owned();
        if username.is_empty() || password.is_empty() {
            return Err(FetchError::InvalidRelease("credentials files must not be empty".into()));
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
    pub size: u64,
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
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            site: Url::parse(DEFAULT_SITE).expect("default Foundry URL is valid"),
            platform: Platform::Node,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
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
        self.root.join(format!("foundryvtt-{}-{}.zip", release, platform_name(platform)))
    }

    pub fn metadata_path(&self, release: &ReleaseId, platform: Platform) -> PathBuf {
        self.root.join(format!("foundryvtt-{}-{}.json", release, platform_name(platform)))
    }

    pub fn get(&self, release: &ReleaseId, platform: Platform) -> Result<Option<Artifact>, FetchError> {
        let archive = self.path_for(release, platform);
        let metadata = self.metadata_path(release, platform);
        if !archive.is_file() || !metadata.is_file() {
            return Ok(None);
        }
        let provenance: Provenance = serde_json::from_slice(&fs::read(&metadata)?)?;
        if provenance.release != *release || provenance.platform != platform {
            return Err(FetchError::InvalidArchive("cache provenance does not match requested release".into()));
        }
        validate_archive(&archive, release, DEFAULT_MAX_BYTES)?;
        Ok(Some(Artifact { archive, provenance }))
    }

    pub fn put(&self, release: &ReleaseId, platform: Platform, source: SourceKind, input: &Path, max_bytes: u64) -> Result<Artifact, FetchError> {
        let (sha256, size) = validate_archive(input, release, max_bytes)?;
        let archive = self.path_for(release, platform);
        let metadata = self.metadata_path(release, platform);
        let mut tmp = NamedTempFile::new_in(&self.root)?;
        copy_file(input, tmp.as_file_mut(), max_bytes)?;
        tmp.as_file().sync_all()?;
        let tmp_path = tmp.into_temp_path();
        fs::rename(&tmp_path, &archive)?;
        let provenance = Provenance { release: release.clone(), platform, source, sha256, size, acquired_at_unix: now_unix() };
        let json = serde_json::to_vec_pretty(&provenance)?;
        let mut meta_tmp = NamedTempFile::new_in(&self.root)?;
        meta_tmp.write_all(&json)?;
        meta_tmp.as_file().sync_all()?;
        let meta_tmp_path = meta_tmp.into_temp_path();
        fs::rename(&meta_tmp_path, &metadata)?;
        set_mode(&archive, 0o600)?;
        set_mode(&metadata, 0o600)?;
        Ok(Artifact { archive, provenance })
    }
}

/// Validate and hash a pre-acquired archive.  The ZIP is checked for a
/// package.json, a matching release, path traversal, and symlink entries.
pub fn validate_archive(path: &Path, release: &ReleaseId, max_bytes: u64) -> Result<(String, u64), FetchError> {
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
        if read == 0 { break; }
        hasher.update(&buf[..read]);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut zip = ZipArchive::new(file).map_err(|e| FetchError::InvalidArchive(e.to_string()))?;
    let mut package_json = None;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| FetchError::InvalidArchive(e.to_string()))?;
        let name = entry.name().replace('\\', "/");
        if !safe_zip_path(&name) {
            return Err(FetchError::InvalidArchive(format!("unsafe ZIP path {name:?}")));
        }
        if is_symlink(&entry) {
            return Err(FetchError::InvalidArchive(format!("symlink ZIP entry {name:?}")));
        }
        if name == "resources/app/package.json" || name == "package.json" {
            let mut bytes = Vec::new();
            let mut reader = entry;
            reader.read_to_end(&mut bytes)?;
            package_json = Some(bytes);
        }
    }
    let package_json = package_json.ok_or_else(|| FetchError::InvalidArchive("missing package.json".into()))?;
    let package: serde_json::Value = serde_json::from_slice(&package_json).map_err(|e| FetchError::InvalidArchive(format!("invalid package.json: {e}")))?;
    if let Some(version) = package.get("version").and_then(serde_json::Value::as_str) {
        if let Ok(found) = ReleaseId::parse(version) {
            if found != *release {
                return Err(FetchError::InvalidArchive(format!("package.json release {found} does not match requested {release}")));
            }
        }
    }
    Ok((hex::encode(hasher.finalize()), meta.len()))
}

/// Acquire an archive through the licensed account flow documented by
/// Foundry's web release endpoint.  Signed URLs remain in memory only.
pub async fn acquire_account(cache: &Cache, release: &ReleaseId, credentials: &AccountCredentials, options: &FetchOptions) -> Result<Artifact, FetchError> {
    let client = Client::builder().cookie_store(true).user_agent("foundryvtt-fetch/0.1").build()?;
    let login = client.get(options.site.join("auth/login/").expect("relative login URL")).send().await?;
    let login_text = checked_text(login).await?;
    let csrf = extract_input(&login_text, "csrfmiddlewaretoken").ok_or(FetchError::MissingFormField("csrfmiddlewaretoken"))?;
    let mut form = std::collections::HashMap::new();
    form.insert("username", credentials.username.as_str());
    form.insert("password", credentials.password.as_str());
    form.insert("csrfmiddlewaretoken", csrf.as_str());
    let response = client.post(options.site.join("auth/login/").expect("relative login URL")).form(&form).send().await?;
    let response_text = checked_text(response).await?;
    if !response_text.contains("login-welcome") && !response_text.contains("logout") {
        return Err(FetchError::Remote { status: StatusCode::UNAUTHORIZED, body: "Foundry account login was rejected".into() });
    }
    let endpoint = options.site.join(&format!("releases/download?build={}&platform={}&response_type=json", release, platform_name(options.platform))).expect("release URL is valid");
    let release_response = client.get(endpoint).send().await?;
    let release_response = checked_json::<ReleaseResponse>(release_response).await?;
    let signed_url = Url::parse(&release_response.url)
        .map_err(|_| FetchError::InvalidArchive("release endpoint returned an invalid download URL".into()))?;
    let response = client.get(signed_url).send().await?;
    if !response.status().is_success() {
        return Err(FetchError::Remote { status: response.status(), body: "Foundry archive download failed".into() });
    }
    let mut tmp = NamedTempFile::new_in(&cache.root)?;
    let mut size = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size = size.saturating_add(chunk.len() as u64);
        if size > options.max_bytes {
            return Err(FetchError::TooLarge { limit: options.max_bytes });
        }
        tmp.write_all(&chunk)?;
    }
    tmp.as_file().sync_all()?;
    let staged = tmp.path().to_path_buf();
    let artifact = cache.put(release, options.platform, SourceKind::LicensedAccount, &staged, options.max_bytes)?;
    Ok(artifact)
}

/// Cache a caller-provided licensed archive after validation.
pub fn acquire_archive(cache: &Cache, release: &ReleaseId, archive: &Path, platform: Platform, max_bytes: u64) -> Result<Artifact, FetchError> {
    cache.put(release, platform, SourceKind::PreAcquiredArchive, archive, max_bytes)
}

async fn checked_text(response: reqwest::Response) -> Result<String, FetchError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(FetchError::Remote { status, body: truncate(body) });
    }
    Ok(body)
}

async fn checked_json<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T, FetchError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(FetchError::Remote { status, body: truncate(body) });
    }
    serde_json::from_str(&body).map_err(FetchError::Metadata)
}

fn extract_input(html: &str, name: &'static str) -> Option<String> {
    let selector = Selector::parse(&format!("input[name='{name}']")).ok()?;
    Html::parse_document(html).select(&selector).find_map(|node| node.value().attr("value").map(ToOwned::to_owned))
}

fn safe_zip_path(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.contains(':') { return false; }
    Path::new(name).components().all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn is_symlink(entry: &zip::read::ZipFile<'_>) -> bool {
    entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000)
}

fn copy_file(input: &Path, output: &mut File, max_bytes: u64) -> Result<(), FetchError> {
    let mut source = File::open(input)?;
    let mut copied = 0_u64;
    let mut buf = [0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buf)?;
        if read == 0 { break; }
        copied = copied.saturating_add(read as u64);
        if copied > max_bytes { return Err(FetchError::TooLarge { limit: max_bytes }); }
        output.write_all(&buf[..read])?;
    }
    Ok(())
}

fn platform_name(platform: Platform) -> &'static str {
    match platform { Platform::Node => "node" }
}

fn truncate(mut value: String) -> String {
    const LIMIT: usize = 512;
    if value.len() > LIMIT { value.truncate(LIMIT); value.push_str("…"); }
    value
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
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
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn archive(path: &Path, package_version: &str) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer.start_file("resources/app/package.json", SimpleFileOptions::default()).unwrap();
        write!(writer, "{{\"version\":\"{package_version}\"}}").unwrap();
        writer.start_file("resources/app/index.js", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"ok").unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn parses_release_names() {
        assert_eq!(ReleaseId::parse("FoundryVTT-Linux-13.351.zip").unwrap(), ReleaseId { major: 13, build: 351 });
        assert_eq!(ReleaseId::parse("13.351").unwrap().archive_name(), "FoundryVTT-Linux-13.351.zip");
        assert!(ReleaseId::parse("13").is_err());
    }

    #[test]
    fn validates_and_hashes_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release.zip");
        archive(&path, "13.351");
        let result = validate_archive(&path, &ReleaseId { major: 13, build: 351 }, 1024 * 1024).unwrap();
        assert_eq!(result.1, fs::metadata(path).unwrap().len());
        assert_eq!(result.0.len(), 64);
    }

    #[test]
    fn rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release.zip");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer.start_file("../escape", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
        assert!(matches!(validate_archive(&path, &ReleaseId { major: 13, build: 351 }, 1024), Err(FetchError::InvalidArchive(_))));
    }
}
