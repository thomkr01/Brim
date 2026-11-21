use anyhow::{Context, Result};

pub struct Config {
    pub discord_token: String,
    pub database_url: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            discord_token: discord_token()?,
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL not set")?,
        })
    }
}

/// Where docker-compose.yml puts the bot token: a file, not the environment, so a leak of the
/// environment (a crash report, a debug page) does not include it (security review, owner decision
/// 2026-11-18).
pub const TOKEN_FILE: &str = "/run/secrets/discord_token";

/// `DISCORD_TOKEN` when set (running without Docker), else [`TOKEN_FILE`].
pub fn discord_token() -> Result<String> {
    if let Some(t) = std::env::var("DISCORD_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
    {
        return Ok(t);
    }
    let t = std::fs::read_to_string(TOKEN_FILE)
        .with_context(|| format!("DISCORD_TOKEN is not set and {TOKEN_FILE} cannot be read"))?;
    let t = t.trim();
    anyhow::ensure!(
        !t.is_empty(),
        "{TOKEN_FILE} is empty: set DISCORD_TOKEN in .env"
    );
    Ok(t.to_string())
}
