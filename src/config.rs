//! `config.toml`: listen address, which hosts to intercept and the plugin switches. Reloaded live.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where the local proxy listens. Keep it on 127.0.0.1.
    pub listen: String,
    /// Hosts whose HTTPS traffic Ashcord opens up (`*.` and `-*` wildcards allowed). Everything else is
    /// tunnelled through untouched, e.g. the CDN, media and voice.
    pub intercept: Vec<String>,
    pub plugins: Plugins,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "127.0.0.1:18463".into(),
            intercept: vec![
                "discord.com".into(),
                "canary.discord.com".into(),
                "ptb.discord.com".into(),
                "discordapp.com".into(),
                "gateway.discord.gg".into(),
                "gateway-*.discord.gg".into(),
            ],
            plugins: Plugins::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugins {
    pub no_track: Toggle<On>,
    pub silent_typing: Toggle,
    pub message_logger: MessageLogger,
    pub fake_nitro: FakeNitro,
    pub relationship_notifier: RelationshipNotifier,
    pub game_activity_toggle: Toggle,
    pub unindent: Toggle,
}

/// A plugin with no options. `Toggle<On>` defaults to enabled.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Toggle<D: DefaultOn = Off> {
    pub enabled: bool,
    #[serde(skip)]
    _d: std::marker::PhantomData<D>,
}

pub trait DefaultOn: Clone + std::fmt::Debug {
    const ON: bool;
}
#[derive(Clone, Debug, Default)]
pub struct On;
#[derive(Clone, Debug, Default)]
pub struct Off;
impl DefaultOn for On {
    const ON: bool = true;
}
impl DefaultOn for Off {
    const ON: bool = false;
}

impl<D: DefaultOn> Default for Toggle<D> {
    fn default() -> Self {
        Toggle { enabled: D::ON, _d: std::marker::PhantomData }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageLogger {
    pub enabled: bool,
    /// Keep your own deleted and edited messages as they are.
    pub ignore_self: bool,
    pub ignore_bots: bool,
    /// Show earlier versions under edited messages.
    pub show_edits: bool,
    /// How many recent messages to remember (needed to show a message after it's deleted).
    pub cache_size: usize,
    /// Also append deleted and edited messages to logs/messages.jsonl (`ashcord log` shows them).
    pub write_log: bool,
}

impl Default for MessageLogger {
    fn default() -> Self {
        MessageLogger {
            enabled: false,
            ignore_self: true,
            ignore_bots: false,
            show_edits: true,
            cache_size: 5000,
            write_log: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FakeNitro {
    pub enabled: bool,
    /// Tell the client you have Nitro so its pickers offer every emoji.
    pub unlock_pickers: bool,
    /// Emoji size in pixels for the links that replace emoji you can't use.
    pub emoji_size: u32,
    /// Send `[name](link)` instead of the bare link.
    pub hyperlinks: bool,
}

impl Default for FakeNitro {
    fn default() -> Self {
        FakeNitro { enabled: false, unlock_pickers: true, emoji_size: 48, hyperlinks: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RelationshipNotifier {
    pub enabled: bool,
    pub friends: bool,
    pub friend_requests: bool,
    pub servers: bool,
    pub groups: bool,
}

impl Default for RelationshipNotifier {
    fn default() -> Self {
        RelationshipNotifier { enabled: false, friends: true, friend_requests: true, servers: true, groups: true }
    }
}

pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    pub fn resolve() -> Paths {
        let config = std::env::var_os("ASHCORD_CONFIG").map(PathBuf::from).unwrap_or_else(|| {
            dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ashcord").join("config.toml")
        });
        let data = std::env::var_os("ASHCORD_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("ashcord"));
        Paths { config, data }
    }

    pub fn message_log(&self) -> PathBuf {
        self.data.join("logs").join("messages.jsonl")
    }
}

impl Config {
    /// Load the config, writing the defaults first if there is none.
    pub fn load_or_init(path: &Path) -> Result<Config> {
        if !path.exists() {
            let c = Config::default();
            c.save(path)?;
            return Ok(c);
        }
        Config::load(path)
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self)?;
        let text = format!(
            "# Ashcord config. Changes apply live (no restart), except `listen`.\n\
             # Plugins: `ashcord plugins`; switch with `ashcord enable NAME` / `ashcord disable NAME`.\n\n{body}"
        );
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Plugin switches as plain JSON, for the `enable`/`disable`/`plugins` commands.
    pub fn plugin_enabled(&self, name: &str) -> Option<bool> {
        let v = serde_json::to_value(&self.plugins).ok()?;
        v.get(name)?.get("enabled")?.as_bool()
    }

    pub fn set_plugin(&mut self, name: &str, on: bool) -> Result<()> {
        let mut v = serde_json::to_value(&self.plugins)?;
        let p = v.get_mut(name).with_context(|| format!("no plugin named {name} (see `ashcord plugins`)"))?;
        p["enabled"] = serde_json::Value::Bool(on);
        self.plugins = serde_json::from_value(v)?;
        Ok(())
    }

    pub fn intercepts(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.intercept.iter().any(|p| host_matches(p, &host))
    }
}

/// `discord.com`, `*.discord.com` (subdomains only) or `gateway-*.discord.gg` (one label wildcard).
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let p = pattern.to_ascii_lowercase();
    if let Some(suffix) = p.strip_prefix("*.") {
        return host.ends_with(&format!(".{suffix}"));
    }
    if let Some((pre, post)) = p.split_once('*') {
        return host.len() > pre.len() + post.len()
            && host.starts_with(pre)
            && host.ends_with(post)
            && !host[pre.len()..host.len() - post.len()].contains('.');
    }
    p == host
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_patterns() {
        assert!(host_matches("discord.com", "discord.com"));
        assert!(!host_matches("discord.com", "cdn.discord.com"));
        assert!(host_matches("*.discord.com", "canary.discord.com"));
        assert!(!host_matches("*.discord.com", "discord.com"));
        assert!(host_matches("gateway-*.discord.gg", "gateway-us-east1-c.discord.gg"));
        assert!(!host_matches("gateway-*.discord.gg", "gateway-x.evil.discord.gg"));
        let c = Config::default();
        assert!(c.intercepts("Gateway.Discord.gg."));
        assert!(!c.intercepts("cdn.discordapp.com"));
        assert!(!c.intercepts("media.discordapp.net"));
    }

    #[test]
    fn defaults_and_toggles_roundtrip() {
        let mut c = Config::default();
        assert_eq!(c.plugin_enabled("no_track"), Some(true));
        assert_eq!(c.plugin_enabled("message_logger"), Some(false));
        c.set_plugin("message_logger", true).unwrap();
        assert!(c.plugins.message_logger.enabled);
        assert!(c.set_plugin("nope", true).is_err());
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert!(back.plugins.message_logger.enabled);
        assert!(back.plugins.no_track.enabled);
        // A config that leaves things out gets the defaults.
        let sparse: Config = toml::from_str("[plugins.unindent]\nenabled = true\n").unwrap();
        assert!(sparse.plugins.no_track.enabled && sparse.plugins.unindent.enabled);
    }
}
