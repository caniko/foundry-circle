//! Command-line front end for the Foundry Circle upload client.

use std::{
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use clap::{Args, Parser, Subcommand};
use foundry_circle_upload::{
    AssetKind, AudioCodec, Error, Options, Session, convert, forget_session, load_session, login,
    save_session, staged_asset, upload, whoami,
};

/// Convert Foundry VTT assets and upload them to a Foundry Circle broker.
#[derive(Debug, Parser)]
#[command(name = "foundry-circle-upload", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sign in through the browser and store the session.
    Login {
        /// Broker base URL, for example https://foundry.example.com.
        #[arg(long)]
        server: String,
    },
    /// Show the stored session's server and principal.
    Whoami,
    /// Delete the stored session from this machine.
    Logout,
    /// Convert assets with ffmpeg and upload them.
    Push(Push),
}

#[derive(Debug, Args)]
struct Push {
    /// Asset kind; format defaults apply when omitted.
    #[arg(long, value_name = "KIND", value_parser = parse_kind)]
    kind: Option<AssetKind>,
    /// Map grid cell in pixels (default 100).
    #[arg(long)]
    grid_px: Option<u32>,
    /// Token side in pixels, 200-400 (default 400).
    #[arg(long)]
    token_px: Option<u32>,
    /// Audio codec for lossless masters.
    #[arg(long, value_name = "CODEC", value_parser = parse_audio)]
    audio: Option<AudioCodec>,
    /// Audio bitrate in kbps, 64-192 (default 128).
    #[arg(long)]
    audio_kbps: Option<u32>,
    /// Convert and print the planned names without uploading.
    #[arg(long)]
    dry_run: bool,
    /// Files to convert and upload.
    #[arg(required = true, value_name = "FILES")]
    files: Vec<PathBuf>,
}

fn parse_kind(value: &str) -> std::result::Result<AssetKind, String> {
    value
        .parse::<AssetKind>()
        .map_err(|error| error.to_string())
}

fn parse_audio(value: &str) -> std::result::Result<AudioCodec, String> {
    match value {
        "vorbis" => Ok(AudioCodec::Vorbis),
        "opus" => Ok(AudioCodec::Opus),
        other => Err(format!("unknown audio codec `{other}`; use vorbis or opus")),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Login { server } => {
            let session = login(&server).await?;
            save_session(&session).with_context(|| {
                format!(
                    "the sign-in succeeded but the session could not be saved to {}",
                    session_path_hint()
                )
            })?;
            println!(
                "Signed in to {} (session expires {}).",
                session.server, session.expires_at
            );
            match whoami(&session).await {
                Ok(principal) => print_principal(&principal),
                Err(error) => eprintln!("could not read the principal: {error}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Whoami => {
            let session = require_session()?;
            let principal = whoami(&session)
                .await
                .map_err(|error| explain(error, &session))?;
            println!("server: {}", session.server);
            println!("expires: {}", session.expires_at);
            print_principal(&principal);
            Ok(ExitCode::SUCCESS)
        }
        Command::Logout => {
            forget_session()?;
            println!(
                "Removed the local session. The server session stays valid until it expires or an admin revokes it."
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Push(push) => push_files(push).await,
    }
}

fn require_session() -> Result<Session> {
    match load_session()? {
        Some(session) => Ok(session),
        None => Err(Error::NoSession.into()),
    }
}

/// Point an expired session at the command that fixes it.
fn explain(error: Error, session: &Session) -> anyhow::Error {
    match error {
        Error::Expired => anyhow::anyhow!(
            "{error}; run `foundry-circle-upload login --server {}`",
            session.server
        ),
        other => other.into(),
    }
}

async fn push_files(push: Push) -> Result<ExitCode> {
    // A dry run only converts, so it works without a session.
    let session = if push.dry_run {
        None
    } else {
        Some(require_session()?)
    };
    let mut options = Options::default();
    if let Some(token_px) = push.token_px {
        options.token_px = token_px;
    }
    if let Some(grid_px) = push.grid_px {
        options.grid_px = grid_px;
    }
    if let Some(audio) = push.audio {
        options.audio = audio;
    }
    if let Some(audio_kbps) = push.audio_kbps {
        options.audio_kbps = audio_kbps;
    }

    let staging = Staging::create()?;
    let mut failed = 0;
    for file in &push.files {
        let converted = match convert(file, push.kind, &options, &staging.path) {
            Ok(converted) => converted,
            Err(error) => {
                eprintln!("{}: {error}", file.display());
                failed += 1;
                continue;
            }
        };
        let Some((kind, relative)) = staged_asset(&staging.path, &converted) else {
            eprintln!(
                "{}: converted to {}, which is not a Foundry asset directory",
                file.display(),
                converted.display()
            );
            failed += 1;
            continue;
        };
        // No session is the dry run: report the planned name and skip the
        // upload. `session` is `Some` for every real push.
        match session.as_ref() {
            None => println!("{} -> {relative}", file.display()),
            Some(session) => match upload(session, kind, &converted).await {
                Ok(path) => println!("{} -> {path}", file.display()),
                Err(Error::Exists) => println!("{} -> exists", file.display()),
                Err(error @ (Error::Expired | Error::Forbidden)) => {
                    eprintln!("{}: {}", file.display(), explain(error, session));
                    return Ok(ExitCode::FAILURE);
                }
                Err(error) => {
                    eprintln!("{}: {error}", file.display());
                    failed += 1;
                }
            },
        }
        // Inputs that normalize to the same name each reach the broker, which
        // decides about duplicates; the staged copy is no longer needed.
        let _ = std::fs::remove_file(&converted);
    }
    if failed > 0 {
        eprintln!("{failed} of {} files failed", push.files.len());
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn print_principal(principal: &serde_json::Value) {
    let display = principal
        .get("display_name")
        .or_else(|| principal.get("displayName"))
        .and_then(serde_json::Value::as_str);
    let subject = principal.get("subject").and_then(serde_json::Value::as_str);
    match (display, subject) {
        (Some(display), Some(subject)) => println!("signed in as {display} ({subject})"),
        (Some(display), None) => println!("signed in as {display}"),
        (None, Some(subject)) => println!("signed in as {subject}"),
        (None, None) => {
            let pretty = serde_json::to_string_pretty(principal).unwrap_or_default();
            println!("{pretty}");
        }
    }
}

fn session_path_hint() -> String {
    foundry_circle_upload::session_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "the per-user config directory".to_string())
}

/// A unique staging directory under the system temp dir, removed on drop.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create() -> Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "foundry-circle-upload-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("cannot create staging directory {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
