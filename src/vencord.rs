//! Bring over your Vencord (or Vesktop / Equicord) plugin choices.

use crate::config::Config;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn find() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    ["Vencord", "vesktop", "Equicord", "equibop"]
        .iter()
        .map(|d| base.join(d).join("settings").join("settings.json"))
        .find(|p| p.is_file())
}

/// Apply the matching plugins from a Vencord settings.json. Returns what changed, for printing.
pub fn import(path: &Path, cfg: &mut Config) -> Result<Vec<String>> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)
        .with_context(|| format!("parsing {}", path.display()))?;
    let plugins = v.get("plugins").context("no plugins in that file")?;
    let on = |name: &str| {
        plugins
            .get(name)
            .and_then(|p| p.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let opt = |name: &str, key: &str| plugins.get(name).and_then(|p| p.get(key)).cloned();
    let before = cfg.clone();

    cfg.plugins.no_track.enabled = on("NoTrack");
    cfg.plugins.silent_typing.enabled = on("SilentTyping");
    cfg.plugins.unindent.enabled = on("Unindent");

    let ml = &mut cfg.plugins.message_logger;
    ml.enabled = on("MessageLogger");
    if let Some(b) = opt("MessageLogger", "ignoreSelf").and_then(|v| v.as_bool()) {
        ml.ignore_self = b;
    }
    if let Some(b) = opt("MessageLogger", "ignoreBots").and_then(|v| v.as_bool()) {
        ml.ignore_bots = b;
    }

    let fnit = &mut cfg.plugins.fake_nitro;
    fnit.enabled = on("FakeNitro");
    if let Some(n) = opt("FakeNitro", "emojiSize").and_then(|v| v.as_u64()) {
        fnit.emoji_size = n as u32;
    }
    if let Some(b) = opt("FakeNitro", "useHyperLinks").and_then(|v| v.as_bool()) {
        fnit.hyperlinks = b;
    }

    let rn = &mut cfg.plugins.relationship_notifier;
    rn.enabled = on("RelationshipNotifier");
    for (key, slot) in [
        ("friends", &mut rn.friends),
        ("friendRequestCancels", &mut rn.friend_requests),
        ("servers", &mut rn.servers),
        ("groups", &mut rn.groups),
    ] {
        if let Some(b) = opt("RelationshipNotifier", key).and_then(|v| v.as_bool()) {
            *slot = b;
        }
    }
    // GameActivityToggle's on/off state lives in Discord's own settings, so it isn't copied.
    let ids = [
        "no_track",
        "silent_typing",
        "unindent",
        "message_logger",
        "fake_nitro",
        "relationship_notifier",
    ];
    Ok(ids
        .iter()
        .filter_map(|id| {
            let now = cfg.plugin_enabled(id)?;
            (before.plugin_enabled(id) != Some(now))
                .then(|| format!("{id}: {}", if now { "on" } else { "off" }))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_matching_plugins() {
        let p = std::env::temp_dir().join(format!("ashcord-vencord-{}.json", std::process::id()));
        std::fs::write(&p, r#"{"plugins":{"NoTrack":{"enabled":true},"MessageLogger":{"enabled":true,"ignoreSelf":false},
            "FakeNitro":{"enabled":true,"emojiSize":64},"ShikiCodeblocks":{"enabled":true}}}"#).unwrap();
        let mut c = Config::default();
        let changes = import(&p, &mut c).unwrap();
        assert!(c.plugins.message_logger.enabled && !c.plugins.message_logger.ignore_self);
        assert_eq!(c.plugins.fake_nitro.emoji_size, 64);
        assert!(!c.plugins.silent_typing.enabled);
        assert!(changes.contains(&"message_logger: on".to_string()));
        std::fs::remove_file(p).ok();
    }
}
