//! Plugins. Each one hooks REST calls (requests and responses) and/or gateway traffic (server events
//! and what the client sends), through the same [`Val`] type whatever the encoding.

use crate::config::{Config, Paths};
use crate::state::Account;
use crate::val::Val;

mod fake_nitro;
mod game_activity;
mod message_logger;
mod no_track;
mod relationship_notifier;
mod silent_typing;
mod unindent;

pub struct Cx<'a> {
    pub cfg: &'a Config,
    pub account: &'a mut Account,
    pub paths: &'a Paths,
}

/// A REST request on its way to Discord. `route` is the path without the `/api/vN` prefix.
pub struct Request {
    pub method: String,
    pub route: String,
    pub body: Option<Val>,
    pub body_changed: bool,
}

/// One server → client gateway payload. Plugins may change it, add payloads after it, or drop it.
pub struct Dispatch {
    pub payload: Val,
    pub extra: Vec<Val>,
    pub changed: bool,
    pub drop: bool,
}

impl Dispatch {
    pub fn event(&self) -> Option<&str> {
        self.payload.str_at(&["t"])
    }
    pub fn data(&mut self) -> Option<&mut Val> {
        self.payload.get_mut("d")
    }
    /// Replace this event with another event type and data, keeping the sequence number.
    pub fn rewrite(&mut self, t: &str, d: Val) {
        let tv = self.payload.get("t").map(|v| v.str_like(t)).unwrap_or(Val::Str(t.into()));
        self.payload.set("t", tv);
        self.payload.set("d", d);
        self.changed = true;
    }
}

pub struct Info {
    pub id: &'static str,
    pub vencord: &'static str,
    pub about: &'static str,
}

pub trait Plugin: Send + Sync {
    fn info(&self) -> Info;
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugin_enabled(self.info().id).unwrap_or(false)
    }
    /// Whether this plugin needs the gateway decoded at all (decoding costs a little CPU).
    fn uses_gateway(&self) -> bool {
        false
    }
    /// Whether the request body (JSON) is needed for this route.
    fn wants_request(&self, _method: &str, _route: &str) -> bool {
        false
    }
    /// Return `Some(status)` to answer locally instead of sending the request to Discord.
    fn on_request(&self, _cx: &mut Cx, _req: &mut Request) -> Option<u16> {
        None
    }
    fn wants_response(&self, _method: &str, _route: &str) -> bool {
        false
    }
    fn on_response(&self, _cx: &mut Cx, _method: &str, _route: &str, _body: &Val) {}
    fn on_dispatch(&self, _cx: &mut Cx, _d: &mut Dispatch) {}
    /// A payload the client sends (op + data). Return true if it was changed.
    fn on_send(&self, _cx: &mut Cx, _payload: &mut Val) -> bool {
        false
    }
}

pub fn all() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(no_track::NoTrack),
        Box::new(silent_typing::SilentTyping),
        Box::new(message_logger::MessageLogger::default()),
        Box::new(fake_nitro::FakeNitro),
        Box::new(relationship_notifier::RelationshipNotifier),
        Box::new(game_activity::GameActivityToggle),
        Box::new(unindent::Unindent),
    ]
}

/// `/api/v9/channels/1/messages` → `/channels/1/messages`; other paths stay as they are.
pub fn route_of(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("/api/") {
        if let Some((ver, tail)) = rest.split_once('/')
            && ver.len() > 1
            && ver.starts_with('v')
            && ver[1..].chars().all(|c| c.is_ascii_digit())
        {
            return format!("/{tail}");
        }
        return format!("/{rest}");
    }
    path.to_string()
}

/// Channel id from `/channels/{id}/messages` (and `/channels/{id}/messages/{mid}`).
pub fn message_route_channel(route: &str) -> Option<u64> {
    let seg: Vec<&str> = route.trim_matches('/').split('/').collect();
    match seg.as_slice() {
        ["channels", id, "messages"] | ["channels", id, "messages", _] => id.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    pub fn j(s: &str) -> Val {
        Val::from_json(serde_json::from_str(s).unwrap())
    }

    pub struct World {
        pub cfg: Config,
        pub account: Account,
        pub paths: Paths,
    }

    impl World {
        pub fn new() -> World {
            let dir = std::env::temp_dir().join(format!("ashcord-test-{}-{:?}", std::process::id(), std::thread::current().id()));
            World {
                cfg: Config::default(),
                account: Account::default(),
                paths: Paths { config: dir.join("config.toml"), data: dir },
            }
        }
        pub fn cx(&mut self) -> Cx<'_> {
            Cx { cfg: &self.cfg, account: &mut self.account, paths: &self.paths }
        }
        pub fn dispatch(&mut self, p: &dyn Plugin, payload: &str) -> Dispatch {
            let mut d = Dispatch { payload: j(payload), extra: vec![], changed: false, drop: false };
            if let (Some(t), Some(data)) = (d.event().map(str::to_string), d.payload.get("d").cloned()) {
                self.account.track(&t, &data);
            }
            p.on_dispatch(&mut self.cx(), &mut d);
            d
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(route_of("/api/v9/channels/1/messages"), "/channels/1/messages");
        assert_eq!(route_of("/api/science"), "/science");
        assert_eq!(route_of("/assets/app.js"), "/assets/app.js");
        assert_eq!(message_route_channel("/channels/42/messages"), Some(42));
        assert_eq!(message_route_channel("/channels/42/messages/7"), Some(42));
        assert_eq!(message_route_channel("/channels/42/typing"), None);
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = all().iter().map(|p| p.info().id).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
        let cfg = Config::default();
        for p in all() {
            assert!(cfg.plugin_enabled(p.info().id).is_some(), "{} has no config section", p.info().id);
        }
    }
}
