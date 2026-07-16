use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use foundryvtt_fetch::{AccountCredentials, Cache, FetchOptions, Platform, ReleaseId, acquire_account, acquire_archive};
use reqwest::Url;

#[derive(Debug, Parser)]
#[command(name = "foundryvtt-fetch", about = "Acquire and validate a licensed Foundry VTT release")]
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
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Archive { release, archive, platform, cache_dir } => {
            let release = ReleaseId::parse(&release).context("parse --release")?;
            let platform = parse_platform(&platform)?;
            let cache = Cache::new(cache_dir).context("create cache")?;
            let artifact = acquire_archive(&cache, &release, &archive, platform, 4 * 1024 * 1024 * 1024).context("cache archive")?;
            println!("archive={} sha256={} size={} release={}", artifact.archive.display(), artifact.provenance.sha256, artifact.provenance.size, artifact.provenance.release);
        }
        Command::Account { release, username_file, password_file, cache_dir, site } => {
            let release = ReleaseId::parse(&release).context("parse --release")?;
            let credentials = AccountCredentials::from_files(username_file, password_file).context("read credentials")?;
            let mut options = FetchOptions::default();
            options.site = Url::parse(&site).context("parse --site")?;
            let cache = Cache::new(cache_dir).context("create cache")?;
            let artifact = acquire_account(&cache, &release, &credentials, &options).await.context("download release")?;
            println!("archive={} sha256={} size={} release={}", artifact.archive.display(), artifact.provenance.sha256, artifact.provenance.size, artifact.provenance.release);
        }
    }
    Ok(())
}

fn parse_platform(value: &str) -> Result<Platform> {
    if value.eq_ignore_ascii_case("node") { Ok(Platform::Node) } else { bail!("unsupported platform {value:?}; only node is supported") }
}
