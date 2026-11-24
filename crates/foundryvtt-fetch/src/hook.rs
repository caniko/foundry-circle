//! Nix `pre-build-hook` client for the declarative licensed source derivation.

use std::{
    borrow::Cow,
    fs::File,
    io::{ErrorKind, Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize};

const MAX_DERIVATION_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const LICENSE_MARKER: &[u8] = b"FOUNDRY_LICENSED_SOURCE";

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
    // Older wrappers still pass this option; never execute it from the hook.
    #[arg(long = "nix", hide = true)]
    _nix: Option<PathBuf>,
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
    let Some(Markers {
        release,
        expected_sri,
    }) = derivation_markers(&parsed.derivation)?
    else {
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

#[derive(Debug, PartialEq, Eq)]
struct Markers {
    release: String,
    expected_sri: String,
}

fn derivation_markers(derivation: &Path) -> Result<Option<Markers>> {
    // Calling Nix here re-enters the daemon while it is waiting for this hook.
    let file = match File::open(derivation) {
        // Nix can also build an in-memory derivation with no local .drv file.
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        result => result.with_context(|| format!("open derivation {}", derivation.display()))?,
    };
    let mut contents = Vec::new();
    file.take((MAX_DERIVATION_BYTES + 1) as u64)
        .read_to_end(&mut contents)
        .with_context(|| format!("read derivation {}", derivation.display()))?;
    // Preserve the previous size-limit behavior: skip oversized derivations.
    // The canonical ATerm writer leaves ASCII marker names unescaped.
    if contents.len() > MAX_DERIVATION_BYTES
        || !contents
            .windows(LICENSE_MARKER.len())
            .any(|window| window == LICENSE_MARKER)
    {
        return Ok(None);
    }
    parse_markers(&contents).with_context(|| format!("parse derivation {}", derivation.display()))
}

fn parse_markers(contents: &[u8]) -> Result<Option<Markers>> {
    let mut term = ATerm(contents);
    term.expect(b"Derive(")?;
    term.list(|term| {
        term.expect(b"(")?;
        term.string()?;
        for _ in 0..3 {
            term.expect(b",")?;
            term.string()?;
        }
        term.expect(b")")
    })?; // outputs
    term.expect(b",")?;
    term.list(|term| {
        term.expect(b"(")?;
        term.string()?;
        term.expect(b",")?;
        term.string_list()?;
        term.expect(b")")
    })?; // inputDrvs
    term.expect(b",")?;
    term.string_list()?; // inputSrcs
    term.expect(b",")?;
    term.string()?; // platform
    term.expect(b",")?;
    term.string()?; // builder
    term.expect(b",")?;
    term.string_list()?; // args
    term.expect(b",")?;

    let (mut licensed, mut release, mut expected_sri) = (false, None, None);
    term.list(|term| {
        term.expect(b"(")?;
        let key = term.string()?;
        term.expect(b",")?;
        let value = term.string()?;
        term.expect(b")")?;
        match key.as_ref() {
            LICENSE_MARKER => licensed = value.as_ref() == b"1",
            b"FOUNDRY_RELEASE" => release = Some(value),
            b"FOUNDRY_EXPECTED_SRI" => expected_sri = Some(value),
            _ => {}
        }
        Ok(())
    })?; // env
    term.expect(b")")?;
    ensure!(term.0.trim_ascii().is_empty(), "trailing data after Derive");
    if !licensed {
        return Ok(None);
    }
    Ok(Some(Markers {
        release: String::from_utf8(
            release
                .context("licensed Foundry derivation has no release marker")?
                .into_owned(),
        )?,
        expected_sri: String::from_utf8(
            expected_sri
                .context("licensed Foundry derivation has no hash marker")?
                .into_owned(),
        )?,
    }))
}

struct ATerm<'a>(&'a [u8]);

impl<'a> ATerm<'a> {
    fn eat(&mut self, token: &[u8]) -> bool {
        if let Some(rest) = self.0.strip_prefix(token) {
            self.0 = rest;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &[u8]) -> Result<()> {
        ensure!(
            self.eat(token),
            "expected {:?}",
            String::from_utf8_lossy(token)
        );
        Ok(())
    }

    fn list(&mut self, mut item: impl FnMut(&mut Self) -> Result<()>) -> Result<()> {
        self.expect(b"[")?;
        if !self.eat(b"]") {
            loop {
                item(self)?;
                if self.eat(b"]") {
                    break;
                }
                self.expect(b",")?;
            }
        }
        Ok(())
    }

    fn string_list(&mut self) -> Result<()> {
        self.list(|term| term.string().map(|_| ()))
    }

    fn string(&mut self) -> Result<Cow<'a, [u8]>> {
        self.expect(b"\"")?;
        let mut decoded = Vec::new();
        let (mut start, mut pos) = (0, 0);
        while let Some(&byte) = self.0.get(pos) {
            match byte {
                b'"' => {
                    let tail = &self.0[start..pos];
                    self.0 = &self.0[pos + 1..];
                    if start == 0 {
                        return Ok(Cow::Borrowed(tail));
                    }
                    decoded.extend_from_slice(tail);
                    return Ok(Cow::Owned(decoded));
                }
                b'\\' => {
                    decoded.extend_from_slice(&self.0[start..pos]);
                    pos += 1;
                    decoded.push(match *self.0.get(pos).context("unterminated escape")? {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        escaped => escaped,
                    });
                    start = pos + 1;
                }
                _ => {}
            }
            pos += 1;
        }
        bail!("unterminated string")
    }
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

    const FOUNDRY_DRV: &str = concat!(
        r#"Derive([("out","/nix/store/00000000000000000000000000000000-test.zip","sha256","0000000000000000000000000000000000000000000000000000000000000000")],"#,
        r#"[("/nix/store/00000000000000000000000000000000-bash.drv",["out"])],"#,
        r#"["/nix/store/00000000000000000000000000000000-builder.sh"],"#,
        r#""x86_64-linux","/nix/store/00000000000000000000000000000000-bash/bin/bash",["-e"],"#,
        r#"[("FOUNDRY_EXPECTED_SRI","sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),"#,
        r#"("FOUNDRY_LICENSED_SOURCE","1"),("FOUNDRY_RELEASE","13.351")])"#,
    );

    fn markers_for(contents: impl AsRef<[u8]>) -> Result<Option<Markers>> {
        let directory = tempfile::tempdir()?;
        let drv = directory.path().join("build.drv");
        std::fs::write(&drv, contents)?;
        derivation_markers(&drv)
    }

    #[test]
    fn reads_foundry_markers_without_nix() -> Result<()> {
        assert_eq!(
            markers_for(FOUNDRY_DRV)?,
            Some(Markers {
                release: "13.351".to_owned(),
                expected_sri: "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned(),
            })
        );
        Ok(())
    }

    #[test]
    fn skips_unrelated_derivations_and_decoy_markers() -> Result<()> {
        for drv in [
            r#"Derive([],[],[],"x86_64-linux","/bin/sh",[],[("name","unrelated")])"#,
            r#"Derive([],[],[],"x86_64-linux","/bin/sh",["FOUNDRY_LICENSED_SOURCE","1"],[("FOUNDRY_RELEASE","13.351"),("FOUNDRY_EXPECTED_SRI","sha256-example")])"#,
            r#"Derive([],[],[],"x86_64-linux","/bin/sh",[],[("description","FOUNDRY_LICENSED_SOURCE"),("FOUNDRY_LICENSED_SOURCE","0")])"#,
        ] {
            assert_eq!(markers_for(drv)?, None);
        }
        Ok(())
    }

    #[test]
    fn reads_escaped_env_values_without_confusing_earlier_fields() -> Result<()> {
        let drv = FOUNDRY_DRV
            .replace(
                r#"["-e"]"#,
                r#"["-e\\","\"],[\"FOUNDRY_LICENSED_SOURCE\",\"\n\r\t"]"#,
            )
            .replace("13.351", r#"13.351\"\\\n\r\t"#);
        assert_eq!(
            markers_for(drv)?.map(|markers| markers.release),
            Some("13.351\"\\\n\r\t".to_owned())
        );
        Ok(())
    }

    #[test]
    fn licensed_derivations_require_both_companion_markers() {
        let without_release = FOUNDRY_DRV.replace("FOUNDRY_RELEASE", "other");
        let without_hash = FOUNDRY_DRV.replace("FOUNDRY_EXPECTED_SRI", "other");
        assert!(
            format!("{:#}", markers_for(without_release).unwrap_err())
                .contains("no release marker")
        );
        assert!(format!("{:#}", markers_for(without_hash).unwrap_err()).contains("no hash marker"));
    }

    #[test]
    fn rejects_malformed_aterm_when_the_marker_is_present() {
        for drv in [
            r#"Derive([],[],[],"x86_64-linux","/bin/sh",[],[("FOUNDRY_LICENSED_SOURCE","1","extra")])"#,
            r#"Derive([],[],[],"x86_64-linux","/bin/sh",[],[("FOUNDRY_LICENSED_SOURCE","\"#,
            r#"Derive([],[],[],"x86_64-linux","/bin/sh",[],[("FOUNDRY_LICENSED_SOURCE","0")])junk"#,
        ] {
            assert!(
                markers_for(drv)
                    .unwrap_err()
                    .to_string()
                    .contains("parse derivation")
            );
        }
    }

    #[test]
    fn skips_derivations_over_the_read_limit() -> Result<()> {
        let mut drv = FOUNDRY_DRV.as_bytes().to_vec();
        drv.resize(MAX_DERIVATION_BYTES + 1, b' ');
        assert_eq!(markers_for(drv)?, None);
        Ok(())
    }

    #[test]
    fn skips_derivations_without_a_local_file() -> Result<()> {
        let directory = tempfile::tempdir()?;
        assert_eq!(
            derivation_markers(&directory.path().join("missing.drv"))?,
            None
        );
        Ok(())
    }

    #[test]
    fn accepts_legacy_nix_flag() {
        assert!(Args::try_parse_from(["hook", "build.drv"]).is_ok());
        assert!(Args::try_parse_from(["hook", "--nix", "/old/bin/nix", "build.drv"]).is_ok());
    }

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
