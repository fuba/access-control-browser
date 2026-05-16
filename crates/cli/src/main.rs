// acb-cli — the operator's command-line surface for the access-controlled
// browser. In M1 only `validate` exists, so the binary has no daemon
// dependency yet. Later milestones add open/snapshot/click/etc.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "acb-cli", version, about = "Access-controlled browser CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check whether a URL is allowed by the policy file. Exits 0 if allowed,
    /// 1 if blocked, 2 on configuration/IO error.
    Validate {
        /// URL to test.
        url: String,
        /// Path to the policy YAML file.
        #[arg(long, short, default_value = "./config.yaml", env = "ACB_CONFIG")]
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();

    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Validate { url, config } => validate(url, config),
    }
}

fn validate(url: String, config: PathBuf) -> Result<ExitCode> {
    let yaml = std::fs::read_to_string(&config)
        .with_context(|| format!("failed to read {}", config.display()))?;
    let cfg = acb_policy::load::load_policy(&yaml)
        .with_context(|| format!("invalid policy in {}", config.display()))?;
    match acb_policy::url_validator::validate_url(&url, &cfg) {
        Ok(acb_policy::url_validator::UrlClass::Allowed { rule_name }) => {
            println!("ALLOWED rule={rule_name}");
            Ok(ExitCode::SUCCESS)
        }
        Err(reason) => {
            eprintln!("BLOCKED {reason}");
            Ok(ExitCode::from(1))
        }
    }
}
