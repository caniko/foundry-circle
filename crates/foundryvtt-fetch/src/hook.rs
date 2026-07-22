//! Nix `pre-build-hook` client for the declarative licensed source derivation.

use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_DERIVATION_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "foundryvtt-fetch-hook",
    about = "Nix pre-build hook for licensed Foundry archives"
)]
struct Args {
    #[arg(value_name = "DERIVATION")]
    derivation: PathBuf,
    #[arg(value_name = "SANDBOX")]
    _sandbox: Option<PathBuf>,
    #[arg(long, default_value = "/run/foundryvtt-acquisition.sock")]
    socket: PathBuf,
    #[arg(long, default_value = "nix")]
    nix: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcquireRequest<'a> {
    schema_version: u32,
    release: &'a str,
    platform: &'static str,
    expected_sri: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcquireResponse {
    schema_version: u32,
    status: String,
    path: Option<PathBuf>,
    error: Option<String>,
}

fn main() -> Result<()> {
    let parsed = Args::parse();
    let markers = derivation_markers(&parsed.nix, &parsed.derivation)?;
    let Some((release, expected_sri)) = markers else {
        return Ok(());
    };
    let request = serde_json::to_vec(&AcquireRequest {
        schema_version: 1,
        release: &release,
        platform: "linux",
        expected_sri: &expected_sri,
    })?;
    let mut stream = std::os::unix::net::UnixStream::connect(&parsed.socket)
        .with_context(|| format!("connect acquisition socket {}", parsed.socket.display()))?;
    stream.write_all(&request)?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES as u64)
        .read_to_end(&mut response)?;
    let response: AcquireResponse =
        serde_json::from_slice(&response).context("decode acquisition response")?;
    if response.schema_version != 1 || response.status != "ready" {
        bail!(
            "licensed Foundry acquisition failed: {}",
            response.error.unwrap_or_else(|| "unknown error".into())
        );
    }
    let path = response
        .path
        .context("acquisition response did not include a cache path")?;
    validate_cache_path(&path)?;
    println!("extra-sandbox-paths");
    println!("/build/foundryvtt-licensed-source={}", path.display());
    println!();
    Ok(())
}

fn derivation_markers(nix: &Path, derivation: &Path) -> Result<Option<(String, String)>> {
    let output = Command::new(nix)
        .args(["derivation", "show", derivation.to_string_lossy().as_ref()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("inspect derivation {}", derivation.display()))?;
    if !output.status.success() || output.stdout.len() > MAX_DERIVATION_BYTES {
        return Ok(None);
    }
    let document: Value =
        serde_json::from_slice(&output.stdout).context("parse derivation metadata")?;
    let derivations = document
        .get("derivations")
        .and_then(Value::as_object)
        .context("derivation metadata has no derivations")?;
    let env = derivations
        .values()
        .next()
        .and_then(|value| value.get("env"))
        .and_then(Value::as_object)
        .context("derivation metadata has no environment")?;
    if env.get("FOUNDRY_LICENSED_SOURCE").and_then(Value::as_str) != Some("1") {
        return Ok(None);
    }
    let release = env
        .get("FOUNDRY_RELEASE")
        .and_then(Value::as_str)
        .context("licensed Foundry derivation has no release marker")?;
    let expected_sri = env
        .get("FOUNDRY_EXPECTED_SRI")
        .and_then(Value::as_str)
        .context("licensed Foundry derivation has no hash marker")?;
    Ok(Some((release.to_owned(), expected_sri.to_owned())))
}

fn validate_cache_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
        || !path.is_file()
    {
        bail!("acquisition response returned an unsafe cache path");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_must_be_absolute_and_regular() {
        assert!(validate_cache_path(Path::new("relative.zip")).is_err());
        assert!(validate_cache_path(Path::new("/var/lib/foundry/../archive.zip")).is_err());
    }

    #[test]
    fn cache_path_accepts_a_regular_absolute_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let archive = directory.path().join("archive.zip");
        std::fs::write(&archive, b"archive").expect("write archive fixture");
        assert!(validate_cache_path(&archive).is_ok());
    }
}
