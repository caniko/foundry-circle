//! Privileged, credential-bearing acquisition service for Nix fixed-output builds.
//!
//! The daemon is deliberately separate from the Nix build hook.  Systemd owns
//! the account password through LoadCredential; build users can only ask for a
//! release and receive a validated, group-readable cache path.

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use foundryvtt_fetch::{
    AccountCredentials, Cache, FetchError, FetchOptions, Platform, ReleaseId, acquire_account,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Mutex,
};
use zeroize::Zeroizing;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "foundryvtt-fetchd",
    about = "Credential-safe Foundry acquisition daemon"
)]
struct Args {
    #[arg(long, default_value = "/run/foundryvtt-acquisition.sock")]
    socket: PathBuf,
    #[arg(long, default_value = "/var/lib/foundryvtt-acquisition")]
    cache_dir: PathBuf,
    #[arg(long)]
    username: String,
    #[arg(long, default_value = "https://foundryvtt.com")]
    site: String,
    #[arg(long, default_value = "account-password")]
    credential_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcquireRequest {
    schema_version: u32,
    release: String,
    platform: Platform,
    expected_sri: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcquireResponse {
    schema_version: u32,
    status: &'static str,
    path: Option<PathBuf>,
    release: Option<String>,
    nix_sri: Option<String>,
    error: Option<String>,
}

impl AcquireResponse {
    fn ready(artifact: &foundryvtt_fetch::Artifact) -> Self {
        Self {
            schema_version: 1,
            status: "ready",
            path: Some(artifact.archive.clone()),
            release: Some(artifact.provenance.release.to_string()),
            nix_sri: Some(artifact.provenance.nix_sri.clone()),
            error: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            status: "error",
            path: None,
            release: None,
            nix_sri: None,
            error: Some(message.into()),
        }
    }
}

fn acquisition_failure(error: FetchError) -> AcquireResponse {
    let message = error.to_string();
    eprintln!("foundryvtt-fetchd: acquisition failed: {message}");
    AcquireResponse::error(message)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let password_file = PathBuf::from(
        env::var_os("CREDENTIALS_DIRECTORY").context("CREDENTIALS_DIRECTORY is required")?,
    )
    .join(&args.credential_name);
    let password = Zeroizing::new(
        fs::read_to_string(&password_file)
            .with_context(|| format!("read systemd credential {}", password_file.display()))?,
    );
    let credentials =
        AccountCredentials::from_values(args.username, password.trim_end().to_owned())
            .context("validate account credential")?;
    let site = Url::parse(&args.site).context("parse Foundry site")?;
    if site.scheme() != "https"
        || site.host_str() != Some("foundryvtt.com")
        || site.port_or_known_default() != Some(443)
    {
        bail!("the acquisition daemon only permits the official https://foundryvtt.com origin");
    }
    let cache = Cache::new(&args.cache_dir).context("create acquisition cache")?;
    cache
        .prepare_shared_read_cache()
        .context("prepare shared acquisition cache")?;

    if let Some(parent) = args.socket.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    let _ = fs::remove_file(&args.socket);
    let listener = UnixListener::bind(&args.socket)
        .with_context(|| format!("bind acquisition socket {}", args.socket.display()))?;
    restrict_socket(&args.socket)?;
    let options = Arc::new(FetchOptions {
        site,
        platform: Platform::Linux,
        max_bytes: 4 * 1024 * 1024 * 1024,
        timeout: Duration::from_secs(120),
        ..FetchOptions::default()
    });
    let lock = Arc::new(Mutex::new(()));
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("accept acquisition request")?;
        let cache = cache.clone();
        let credentials = credentials.clone();
        let options = Arc::clone(&options);
        let lock = Arc::clone(&lock);
        tokio::spawn(async move {
            let _ = handle(stream, cache, credentials, options, lock).await;
        });
    }
}

async fn handle(
    stream: UnixStream,
    cache: Cache,
    credentials: AccountCredentials,
    options: Arc<FetchOptions>,
    lock: Arc<Mutex<()>>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(read).take(MAX_REQUEST_BYTES as u64);
    reader
        .read_line(&mut line)
        .await
        .context("read acquisition request")?;
    let response = match serde_json::from_str::<AcquireRequest>(line.trim_end()) {
        Ok(request) => acquire(request, &cache, &credentials, &options, &lock).await,
        Err(error) => AcquireResponse::error(format!("invalid request: {error}")),
    };
    let mut encoded = serde_json::to_vec(&response).context("encode acquisition response")?;
    encoded.push(b'\n');
    if encoded.len() > MAX_RESPONSE_BYTES {
        bail!("acquisition response exceeded the protocol limit");
    }
    write
        .write_all(&encoded)
        .await
        .context("write acquisition response")?;
    Ok(())
}

async fn acquire(
    request: AcquireRequest,
    cache: &Cache,
    credentials: &AccountCredentials,
    options: &FetchOptions,
    lock: &Mutex<()>,
) -> AcquireResponse {
    if request.schema_version != 1 {
        return AcquireResponse::error("unsupported acquisition protocol version");
    }
    if request.platform != Platform::Linux {
        return AcquireResponse::error(
            "only the Linux Foundry archive is licensed by this service",
        );
    }
    let release = match ReleaseId::parse(&request.release) {
        Ok(release) => release,
        Err(_) => return AcquireResponse::error("invalid Foundry release identifier"),
    };
    if request.expected_sri.is_empty() || !request.expected_sri.starts_with("sha256-") {
        return AcquireResponse::error("expected_sri must be a sha256 SRI digest");
    }
    let _guard = lock.lock().await;
    let artifact = match cache.get_with_max(&release, Platform::Linux, options.max_bytes) {
        Ok(Some(artifact)) => Ok(artifact),
        Ok(None) => acquire_account(cache, &release, credentials, options).await,
        Err(error) => Err(error),
    };
    match artifact {
        Ok(artifact) if artifact.provenance.nix_sri == request.expected_sri => {
            if cache.prepare_shared_read_cache().is_err() {
                return AcquireResponse::error(
                    "cannot publish the validated archive to the Nix build group",
                );
            }
            AcquireResponse::ready(&artifact)
        }
        Ok(_) => AcquireResponse::error("cached archive hash does not match the declared Nix hash"),
        Err(error) => acquisition_failure(error),
    }
}

fn restrict_socket(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn endpoint_statuses_reach_the_daemon_error() {
        for (status, context) in [
            (StatusCode::UNAUTHORIZED, "account login"),
            (StatusCode::FORBIDDEN, "release URL endpoint"),
            (StatusCode::TOO_MANY_REQUESTS, "release URL endpoint"),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "downloading Foundry archive",
            ),
        ] {
            let response = acquisition_failure(FetchError::Remote {
                status,
                context: context.into(),
            });
            let message = response.error.expect("error response has a message");
            assert!(message.contains(status.as_str()), "{message}");
            assert!(message.contains(context), "{message}");
        }

        let response = acquisition_failure(FetchError::AuthenticationRejected {
            status: StatusCode::OK,
        });
        let message = response.error.expect("error response has a message");
        assert!(message.contains("200 OK"), "{message}");
    }
}
