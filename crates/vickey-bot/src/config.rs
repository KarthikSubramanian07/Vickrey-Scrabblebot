//! Connection settings, the bot token file, and strategy config loading.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use vickey_brain::{FieldModel, Params};

/// Options every subcommand shares.
#[derive(clap::Args, Clone, Debug)]
pub struct Common {
    /// SpacetimeDB host.
    #[arg(
        long,
        env = "STDB_HOST",
        default_value = "https://maincloud.spacetimedb.com",
        global = true
    )]
    pub host: String,
    /// Database name.
    #[arg(long, env = "STDB_DB", default_value = "scrabblebot", global = true)]
    pub db: String,
    /// Where the bot's token lives (default: $HOME/.config/vickey/token).
    #[arg(long, env = "VICKEY_TOKEN_FILE", global = true)]
    pub token_file: Option<PathBuf>,
    /// Directory for telemetry, heartbeat and logs.
    #[arg(long, env = "VICKEY_RUNS_DIR", default_value = "runs", global = true)]
    pub runs_dir: PathBuf,
    /// Directory holding params.json / field.json (hot-reloaded each match).
    #[arg(
        long,
        env = "VICKEY_CONFIG_DIR",
        default_value = "config",
        global = true
    )]
    pub config_dir: PathBuf,
}

impl Common {
    pub fn token_path(&self) -> PathBuf {
        self.token_file.clone().unwrap_or_else(default_token_path)
    }

    /// `BOT_TOKEN` wins over the token file. `None` means connect anonymously.
    pub fn bot_token(&self) -> Result<Option<String>> {
        if let Ok(t) = std::env::var("BOT_TOKEN") {
            let t = t.trim().to_string();
            if !t.is_empty() {
                return Ok(Some(t));
            }
        }
        read_token(&self.token_path())
    }

    pub fn params(&self) -> Params {
        load_params(&self.config_dir)
    }

    /// Heads-up profile (`params_duel.json`), if present and valid.
    pub fn duel_params(&self) -> Option<Params> {
        load_duel_params(&self.config_dir)
    }

    pub fn field(&self) -> FieldModel {
        load_field(&self.config_dir)
    }

    /// Live A/B experiment (`ab.json`), if one is configured.
    pub fn ab(&self) -> Option<AbConfig> {
        load_ab(&self.config_dir)
    }
}

/// A live A/B test: casual matches are split between the champion
/// (`params.json`) and a challenger file. Tournament matches always use the
/// champion. Assignment hashes the match id, so a restart mid-match keeps
/// the same arm.
#[derive(Clone, Debug, serde::Deserialize, PartialEq)]
pub struct AbConfig {
    /// Challenger params file, relative to the config dir.
    pub challenger: String,
    /// Share of casual matches that use the challenger.
    #[serde(default = "half")]
    pub share: f64,
    /// Free-form label written to telemetry.
    #[serde(default)]
    pub name: String,
}

fn half() -> f64 {
    0.5
}

pub fn load_ab(dir: &Path) -> Option<AbConfig> {
    let path = dir.join("ab.json");
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<AbConfig>(&text) {
        Ok(ab) if dir.join(&ab.challenger).exists() => Some(ab),
        Ok(ab) => {
            tracing::warn!("ab.json names missing challenger {}", ab.challenger);
            None
        }
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", path.display());
            None
        }
    }
}

/// Whether `match_id` goes to the challenger arm.
pub fn ab_pick(match_id: u64, share: f64) -> bool {
    let mut x = match_id.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    (x % 10_000) as f64 / 10_000.0 < share
}

/// Params for a casual match under an A/B test: `(params, arm label)`.
pub fn ab_params(dir: &Path, ab: &AbConfig, match_id: u64) -> Option<(Params, String)> {
    if !ab_pick(match_id, ab.share) {
        return None;
    }
    let path = dir.join(&ab.challenger);
    match Params::load(&path) {
        Ok(p) => Some((p, format!("B:{}", ab.challenger))),
        Err(e) => {
            tracing::warn!("ignoring challenger {}: {e}", path.display());
            None
        }
    }
}

pub fn default_token_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("vickey").join("token")
}

/// Reads a token file; a missing or empty file is `Ok(None)`.
pub fn read_token(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let t = s.trim().to_string();
            Ok((!t.is_empty()).then_some(t))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading token file {}", path.display())),
    }
}

/// Writes a token with mode 0600, creating the parent directory (0700).
pub fn save_token(path: &Path, token: &str) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(token.trim().as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// `config/params.json` if present and valid, else the defaults.
pub fn load_params(dir: &Path) -> Params {
    let path = dir.join("params.json");
    if !path.exists() {
        return Params::default();
    }
    match Params::load(&path) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", path.display());
            Params::default()
        }
    }
}

/// `config/params_duel.json` if present and valid, else `None` (the brain
/// then uses the main profile at heads-up tables too).
pub fn load_duel_params(dir: &Path) -> Option<Params> {
    let path = dir.join("params_duel.json");
    if !path.exists() {
        return None;
    }
    match Params::load(&path) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", path.display());
            None
        }
    }
}

/// `config/field.json` if present and valid, else the built-in fit.
pub fn load_field(dir: &Path) -> FieldModel {
    let path = dir.join("field.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return FieldModel::default();
    };
    match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("ignoring {}: {e}", path.display());
            FieldModel::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vickey-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn token_roundtrip_is_private() {
        let dir = scratch("tok");
        let path = dir.join("nested").join("token");
        assert_eq!(read_token(&path).unwrap(), None);
        save_token(&path, "  abc.def.ghi \n").unwrap();
        assert_eq!(read_token(&path).unwrap().as_deref(), Some("abc.def.ghi"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_falls_back_to_defaults() {
        let dir = scratch("cfg");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_params(&dir), Params::default());
        std::fs::write(dir.join("params.json"), "{\"samples\": 7}").unwrap();
        assert_eq!(load_params(&dir).samples, 7);
        std::fs::write(dir.join("params.json"), "not json").unwrap();
        assert_eq!(load_params(&dir), Params::default());
        assert_eq!(load_duel_params(&dir), None);
        std::fs::write(dir.join("params_duel.json"), "{\"deny_weight\": 1.25}").unwrap();
        assert_eq!(load_duel_params(&dir).map(|p| p.deny_weight), Some(1.25));
        std::fs::write(dir.join("params_duel.json"), "[oops").unwrap();
        assert_eq!(load_duel_params(&dir), None);
        std::fs::write(dir.join("field.json"), "{}").unwrap();
        assert_eq!(load_field(&dir).sigma, FieldModel::default().sigma);
        let _ = std::fs::remove_dir_all(dir);
    }
}
