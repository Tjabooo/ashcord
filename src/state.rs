//! What Ashcord knows about the signed-in account, learned from the gateway: your user, your servers
//! and their emoji, which channel belongs to which server, and names for notifications.

use crate::val::Val;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Account {
    pub user_id: Option<u64>,
    /// The real Nitro tier (0 = none), before Fake Nitro changes what the client sees.
    pub premium_type: i64,
    pub guild_emojis: HashMap<u64, HashSet<u64>>,
    pub channel_guild: HashMap<u64, u64>,
    pub guild_names: HashMap<u64, String>,
    pub group_names: HashMap<u64, String>,
    pub user_names: HashMap<u64, String>,
    self_actions: HashMap<String, Instant>,
}

fn display_name(u: &Val) -> Option<String> {
    u.str_at(&["global_name"])
        .or_else(|| u.str_at(&["username"]))
        .map(str::to_string)
}

impl Account {
    pub fn is_me(&self, id: Option<u64>) -> bool {
        id.is_some() && id == self.user_id
    }

    pub fn name_of(&self, user: u64) -> String {
        self.user_names
            .get(&user)
            .cloned()
            .unwrap_or_else(|| format!("user {user}"))
    }

    fn user(&mut self, u: &Val) {
        if let (Some(id), Some(name)) = (u.id_at(&["id"]), display_name(u)) {
            self.user_names.insert(id, name);
        }
    }

    fn guild(&mut self, g: &Val) {
        let Some(id) = g.id_at(&["id"]) else { return };
        if let Some(name) = g
            .str_at(&["name"])
            .or_else(|| g.str_at(&["properties", "name"]))
        {
            self.guild_names.insert(id, name.to_string());
        }
        if let Some(emojis) = g.get("emojis") {
            self.guild_emojis.insert(
                id,
                emojis
                    .as_list()
                    .iter()
                    .filter_map(|e| e.id_at(&["id"]))
                    .collect(),
            );
        }
        for key in ["channels", "threads"] {
            for c in g.get(key).map(Val::as_list).unwrap_or_default() {
                if let Some(cid) = c.id_at(&["id"]) {
                    self.channel_guild.insert(cid, id);
                }
            }
        }
    }

    fn private_channel(&mut self, c: &Val) {
        let Some(id) = c.id_at(&["id"]) else { return };
        if c.at(&["type"]).and_then(Val::as_i64) != Some(3) {
            return;
        }
        for r in c.get("recipients").map(Val::as_list).unwrap_or_default() {
            self.user(r);
        }
        let name = c
            .str_at(&["name"])
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                let names: Vec<String> = c
                    .get("recipients")
                    .map(Val::as_list)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(display_name)
                    .collect();
                if names.is_empty() {
                    "a group DM".into()
                } else {
                    names.join(", ")
                }
            });
        self.group_names.insert(id, name);
    }

    /// Update from a gateway dispatch, before plugins see it.
    pub fn track(&mut self, t: &str, d: &Val) {
        match t {
            "READY" => {
                if let Some(u) = d.get("user") {
                    self.user_id = u.id_at(&["id"]);
                    self.premium_type = u.at(&["premium_type"]).and_then(Val::as_i64).unwrap_or(0);
                    self.user(u);
                }
                for u in d.get("users").map(Val::as_list).unwrap_or_default() {
                    self.user(u);
                }
                for g in d.get("guilds").map(Val::as_list).unwrap_or_default() {
                    self.guild(g);
                }
                for c in d
                    .get("private_channels")
                    .map(Val::as_list)
                    .unwrap_or_default()
                {
                    self.private_channel(c);
                }
                for r in d.get("relationships").map(Val::as_list).unwrap_or_default() {
                    if let Some(u) = r.get("user") {
                        self.user(u);
                    }
                }
            }
            "GUILD_CREATE" | "GUILD_UPDATE" => self.guild(d),
            "GUILD_EMOJIS_UPDATE" => {
                if let Some(g) = d.id_at(&["guild_id"]) {
                    let ids = d.get("emojis").map(Val::as_list).unwrap_or_default();
                    self.guild_emojis
                        .insert(g, ids.iter().filter_map(|e| e.id_at(&["id"])).collect());
                }
            }
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "THREAD_CREATE" | "THREAD_UPDATE"
            | "MESSAGE_CREATE" => {
                let cid = if t == "MESSAGE_CREATE" {
                    d.id_at(&["channel_id"])
                } else {
                    d.id_at(&["id"])
                };
                match (cid, d.id_at(&["guild_id"])) {
                    (Some(c), Some(g)) => {
                        self.channel_guild.insert(c, g);
                    }
                    _ if t.starts_with("CHANNEL") => self.private_channel(d),
                    _ => {}
                }
                if let Some(a) = d.get("author") {
                    self.user(a);
                }
            }
            "THREAD_LIST_SYNC" => {
                let g = d.id_at(&["guild_id"]);
                for th in d.get("threads").map(Val::as_list).unwrap_or_default() {
                    if let (Some(c), Some(g)) =
                        (th.id_at(&["id"]), g.or_else(|| th.id_at(&["guild_id"])))
                    {
                        self.channel_guild.insert(c, g);
                    }
                }
            }
            "RELATIONSHIP_ADD" => {
                if let Some(u) = d.get("user") {
                    self.user(u);
                }
            }
            "USER_UPDATE" if self.is_me(d.id_at(&["id"])) => {
                if let Some(p) = d.at(&["premium_type"]).and_then(Val::as_i64) {
                    self.premium_type = p;
                }
                self.user(d);
            }
            _ => {}
        }
    }

    /// Remember things you did yourself, so notifications don't report your own actions.
    pub fn record_request(&mut self, method: &str, route: &str, body: Option<&Val>) {
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();
        let key = match (method, seg.as_slice()) {
            ("DELETE", ["users", "@me", "relationships", id]) => Some(format!("friend:{id}")),
            ("PUT", ["users", "@me", "relationships", id])
                if body.and_then(|b| b.at(&["type"])).and_then(Val::as_i64) == Some(2) =>
            {
                Some(format!("friend:{id}"))
            }
            ("DELETE", ["users", "@me", "guilds", id]) | ("DELETE", ["guilds", id]) => {
                Some(format!("guild:{id}"))
            }
            ("POST", ["guilds", id, "delete"]) => Some(format!("guild:{id}")),
            ("DELETE", ["channels", id]) => Some(format!("group:{id}")),
            _ => None,
        };
        if let Some(k) = key {
            self.self_actions.insert(k, Instant::now());
        }
        self.self_actions
            .retain(|_, t| t.elapsed() < Duration::from_secs(120));
    }

    pub fn did_myself(&self, key: &str) -> bool {
        self.self_actions
            .get(key)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(s: &str) -> Val {
        Val::from_json(serde_json::from_str(s).unwrap())
    }

    #[test]
    fn learns_from_ready() {
        let mut a = Account::default();
        a.track(
            "READY",
            &j(r#"{"user":{"id":"1","username":"me","premium_type":0},
                  "guilds":[{"id":"10","properties":{"name":"Ember"},"emojis":[{"id":"100"}],"channels":[{"id":"11"}]}],
                  "private_channels":[{"id":"20","type":3,"name":"","recipients":[{"id":"2","username":"bo"}]}],
                  "relationships":[{"id":"3","type":1,"user":{"id":"3","username":"x","global_name":"Ex"}}]}"#),
        );
        assert!(a.is_me(Some(1)));
        assert_eq!(a.channel_guild.get(&11), Some(&10));
        assert!(a.guild_emojis[&10].contains(&100));
        assert_eq!(a.guild_names[&10], "Ember");
        assert_eq!(a.group_names[&20], "bo");
        assert_eq!(a.name_of(3), "Ex");
    }

    #[test]
    fn remembers_own_actions() {
        let mut a = Account::default();
        a.record_request("DELETE", "/users/@me/guilds/55", None);
        assert!(a.did_myself("guild:55"));
        assert!(!a.did_myself("guild:56"));
    }
}
