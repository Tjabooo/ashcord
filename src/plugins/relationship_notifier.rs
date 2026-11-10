//! A desktop notification when someone unfriends you, cancels a friend request, or removes you from a
//! server or group DM (Vencord: RelationshipNotifier). Things you did yourself are not reported.

use super::{Cx, Dispatch, Info, Plugin};
use crate::config::Config;
use crate::val::Val;

pub struct RelationshipNotifier;

/// The notification for an event, if any: (title, body).
pub fn notice(cx: &Cx, t: &str, d: &Val) -> Option<(String, String)> {
    let o = &cx.cfg.plugins.relationship_notifier;
    let a = &cx.account;
    match t {
        "RELATIONSHIP_REMOVE" => {
            let id = d.id_at(&["id"])?;
            if a.did_myself(&format!("friend:{id}")) {
                return None;
            }
            let who = a.name_of(id);
            match d.at(&["type"]).and_then(Val::as_i64) {
                Some(1) if o.friends => Some((
                    "Friend removed".into(),
                    format!("{who} is no longer your friend."),
                )),
                Some(3) if o.friend_requests => Some((
                    "Friend request cancelled".into(),
                    format!("{who} cancelled their friend request."),
                )),
                Some(4) if o.friend_requests => Some((
                    "Friend request declined".into(),
                    format!("{who} declined your friend request."),
                )),
                _ => None,
            }
        }
        "GUILD_DELETE" if o.servers => {
            let id = d.id_at(&["id"])?;
            if d.at(&["unavailable"]).and_then(Val::as_bool) == Some(true)
                || a.did_myself(&format!("guild:{id}"))
            {
                return None;
            }
            let name = a
                .guild_names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("server {id}"));
            Some((
                "Removed from a server".into(),
                format!(
                    "You're no longer in {name}. You were kicked or banned, or it was deleted."
                ),
            ))
        }
        "CHANNEL_RECIPIENT_REMOVE" if o.groups => {
            let ch = d.id_at(&["channel_id"])?;
            if !a.is_me(d.id_at(&["user", "id"])) || a.did_myself(&format!("group:{ch}")) {
                return None;
            }
            let name = a
                .group_names
                .get(&ch)
                .cloned()
                .unwrap_or_else(|| "a group DM".into());
            Some((
                "Removed from a group".into(),
                format!("You were removed from {name}."),
            ))
        }
        _ => None,
    }
}

impl Plugin for RelationshipNotifier {
    fn info(&self) -> Info {
        Info {
            id: "relationship_notifier",
            vencord: "RelationshipNotifier",
            about: "Notify you when you're unfriended or removed from a server or group",
        }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugins.relationship_notifier.enabled
    }
    fn uses_gateway(&self) -> bool {
        true
    }
    fn on_dispatch(&self, cx: &mut Cx, d: &mut Dispatch) {
        let (Some(t), Some(data)) = (d.event(), d.payload.get("d")) else {
            return;
        };
        if let Some((title, body)) = notice(cx, t, data) {
            crate::notify::desktop(&title, &body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::testing::{World, j};

    #[test]
    fn reports_removals_but_not_own_actions() {
        let mut w = World::new();
        w.account.track(
            "READY",
            &j(r#"{"user":{"id":"1"},"guilds":[{"id":"10","name":"Ember"}],
            "relationships":[{"id":"3","type":1,"user":{"id":"3","username":"bo"}}]}"#),
        );
        let n = notice(&w.cx(), "RELATIONSHIP_REMOVE", &j(r#"{"id":"3","type":1}"#)).unwrap();
        assert_eq!(n.1, "bo is no longer your friend.");
        let n = notice(&w.cx(), "GUILD_DELETE", &j(r#"{"id":"10"}"#)).unwrap();
        assert!(n.1.contains("Ember"));
        assert!(
            notice(
                &w.cx(),
                "GUILD_DELETE",
                &j(r#"{"id":"10","unavailable":true}"#)
            )
            .is_none()
        );
        w.account
            .record_request("DELETE", "/users/@me/guilds/10", None);
        assert!(notice(&w.cx(), "GUILD_DELETE", &j(r#"{"id":"10"}"#)).is_none());
        w.account
            .record_request("DELETE", "/users/@me/relationships/3", None);
        assert!(notice(&w.cx(), "RELATIONSHIP_REMOVE", &j(r#"{"id":"3","type":1}"#)).is_none());
    }
}
