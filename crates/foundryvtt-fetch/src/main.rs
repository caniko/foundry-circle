use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use foundryvtt_fetch::{
    AccountCredentials, AcquireSources, Artifact, Cache, FetchOptions, Platform, ReleaseId,
    acquire, acquire_account, acquire_archive, reconcile::reconcile_files,
};
use reqwest::Url;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "foundryvtt-fetch",
    about = "Acquire and validate a licensed Foundry VTT release"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate and cache an archive already acquired from Foundry.
    Archive {
        #[arg(long)]
        release: String,
        #[arg(long)]
        archive: PathBuf,
        #[arg(long, default_value = "node")]
        platform: String,
        #[arg(long, default_value = "./foundry-cache")]
        cache_dir: PathBuf,
    },
    /// Log in using credential files and cache a release without exposing
    /// credentials or the signed URL in process arguments or output.
    Account {
        #[arg(long)]
        release: String,
        #[arg(long)]
        username_file: PathBuf,
        #[arg(long)]
        password_file: PathBuf,
        #[arg(long, default_value = "./foundry-cache")]
        cache_dir: PathBuf,
        #[arg(long, default_value = "https://foundryvtt.com")]
        site: String,
    },
    /// Acquire using URL-file, account-file, then exact-cache precedence.
    Acquire {
        #[arg(long)]
        release: String,
        #[arg(long, default_value = "./foundry-cache")]
        cache_dir: PathBuf,
        #[arg(long)]
        release_url_file: Option<PathBuf>,
        #[arg(long)]
        username_file: Option<PathBuf>,
        #[arg(long)]
        password_file: Option<PathBuf>,
        #[arg(long)]
        offline: bool,
        #[arg(long, default_value = "https://foundryvtt.com")]
        site: String,
    },
    /// Reconcile immutable module/system links from a Nix-generated manifest.
    Reconcile {
        #[arg(long)]
        desired: PathBuf,
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        state_file: PathBuf,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactOutput<'a> {
    schema_version: u32,
    release: &'a ReleaseId,
    platform: Platform,
    source: foundryvtt_fetch::SourceKind,
    archive: &'a PathBuf,
    sha256: &'a str,
    nix_sri: &'a str,
    size: u64,
    acquired_at: &'a str,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Archive {
            release,
            archive,
            platform,
            cache_dir,
        } => {
            let release = ReleaseId::parse(&release).context("parse --release")?;
            let platform = parse_platform(&platform)?;
            let cache = Cache::new(cache_dir).context("create cache")?;
            let artifact =
                acquire_archive(&cache, &release, &archive, platform, 4 * 1024 * 1024 * 1024)
                    .context("cache archive")?;
            print_artifact(&artifact)?;
        }
        Command::Account {
            release,
            username_file,
            password_file,
            cache_dir,
            site,
        } => {
            let release = ReleaseId::parse(&release).context("parse --release")?;
            let credentials = AccountCredentials::from_files(username_file, password_file)
                .context("read credentials")?;
            let options = FetchOptions {
                site: Url::parse(&site).context("parse --site")?,
                ..FetchOptions::default()
            };
            let cache = Cache::new(cache_dir).context("create cache")?;
            let artifact = acquire_account(&cache, &release, &credentials, &options)
                .await
                .context("download release")?;
            print_artifact(&artifact)?;
        }
        Command::Acquire {
            release,
            cache_dir,
            release_url_file,
            username_file,
            password_file,
            offline,
            site,
        } => {
            let release = ReleaseId::parse(&release).context("parse --release")?;
            let options = FetchOptions {
                site: Url::parse(&site).context("parse --site")?,
                ..FetchOptions::default()
            };
            let cache = Cache::new(cache_dir).context("create cache")?;
            let sources = AcquireSources {
                release_url_file,
                username_file,
                password_file,
                offline,
            };
            let artifact = acquire(&cache, &release, &sources, &options)
                .await
                .context("acquire release")?;
            print_artifact(&artifact)?;
        }
        Command::Reconcile {
            desired,
            data_dir,
            state_file,
        } => {
            reconcile_files(&desired, &data_dir, &state_file)
                .context("reconcile Foundry packages")?;
        }
    }
    Ok(())
}

fn print_artifact(artifact: &Artifact) -> Result<()> {
    let output = ArtifactOutput {
        schema_version: 1,
        release: &artifact.provenance.release,
        platform: artifact.provenance.platform,
        source: artifact.provenance.source,
        archive: &artifact.archive,
        sha256: &artifact.provenance.sha256,
        nix_sri: &artifact.provenance.nix_sri,
        size: artifact.provenance.size,
        acquired_at: &artifact.provenance.acquired_at,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn parse_platform(value: &str) -> Result<Platform> {
    if value.eq_ignore_ascii_case("node") {
        Ok(Platform::Node)
    } else {
        bail!("unsupported platform {value:?}; only node is supported")
    }
}
