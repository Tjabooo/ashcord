//! Use any emoji without Nitro (Vencord: FakeNitro). Emoji you can't send for real are swapped for a
//! small linked image just before the message leaves, so they show up for everyone.

use super::{Cx, Dispatch, Info, Plugin, Request, message_route_channel};
use crate::config::Config;
use crate::val::Val;
use regex::Regex;
use std::sync::LazyLock;

pub struct FakeNitro;

static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(a?):([A-Za-z0-9_~]{1,64}):(\d{5,25})>").unwrap());

/// Rewrite `<:name:id>` emoji the account can't use in this place. `usable(id)` says which it can.
pub fn rewrite(content: &str, usable: impl Fn(u64) -> bool, size: u32, hyperlinks: bool) -> String {
    EMOJI
        .replace_all(content, |c: &regex::Captures| {
            let id: u64 = c[3].parse().unwrap_or(0);
            if usable(id) {
                return c[0].to_string();
            }
            let ext = if &c[1] == "a" { "gif" } else { "webp" };
            let url = format!("https://cdn.discordapp.com/emojis/{id}.{ext}?size={size}&name={}", &c[2]);
            if hyperlinks { format!("[{}]({url})", &c[2]) } else { url }
        })
        .into_owned()
}

impl Plugin for FakeNitro {
    fn info(&self) -> Info {
        Info { id: "fake_nitro", vencord: "FakeNitro", about: "Send any emoji without Nitro (as linked images)" }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugins.fake_nitro.enabled
    }
    fn uses_gateway(&self) -> bool {
        true
    }
    fn wants_request(&self, method: &str, route: &str) -> bool {
        matches!(method, "POST" | "PATCH") && message_route_channel(route).is_some()
    }
    fn on_request(&self, cx: &mut Cx, req: &mut Request) -> Option<u16> {
        if !self.wants_request(&req.method, &req.route) || cx.account.premium_type > 0 {
            return None;
        }
        let channel = message_route_channel(&req.route)?;
        let guild = cx.account.channel_guild.get(&channel).copied();
        // Until the gateway's READY has arrived Ashcord doesn't know your servers: leave messages alone.
        cx.account.user_id?;
        let allowed = guild.and_then(|g| cx.account.guild_emojis.get(&g));
        let body = req.body.as_mut()?;
        let text = body.str_at(&["content"])?;
        let o = &cx.cfg.plugins.fake_nitro;
        let fixed = rewrite(text, |id| allowed.is_some_and(|set| set.contains(&id)), o.emoji_size, o.hyperlinks);
        if fixed != text {
            body.set("content", Val::Str(fixed));
            req.body_changed = true;
        }
        None
    }
    fn on_dispatch(&self, cx: &mut Cx, d: &mut Dispatch) {
        if !cx.cfg.plugins.fake_nitro.unlock_pickers || cx.account.premium_type > 0 {
            return;
        }
        let user_path: &[&str] = match d.event() {
            Some("READY") => &["user"],
            Some("USER_UPDATE") if cx.account.is_me(d.payload.id_at(&["d", "id"])) => &[],
            _ => return,
        };
        let Some(mut user) = d.data() else { return };
        for k in user_path {
            match user.get_mut(k) {
                Some(u) => user = u,
                None => return,
            }
        }
        user.set("premium_type", Val::Int(2));
        d.changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::testing::World;

    #[test]
    fn rewrites_only_unusable_emoji() {
        let s = "hi <:ember:111111> and <a:spin:222222>";
        let out = rewrite(s, |id| id == 111111, 48, true);
        assert_eq!(out, "hi <:ember:111111> and [spin](https://cdn.discordapp.com/emojis/222222.gif?size=48&name=spin)");
        assert_eq!(rewrite("<:x:333333>", |_| false, 64, false), "https://cdn.discordapp.com/emojis/333333.webp?size=64&name=x");
    }

    #[test]
    fn end_to_end() {
        let mut w = World::new();
        w.cfg.plugins.fake_nitro.enabled = true;
        let ready = w.dispatch(&FakeNitro, r#"{"op":0,"t":"READY","d":{"user":{"id":"1","premium_type":0},
            "guilds":[{"id":"10","emojis":[{"id":"111111"}],"channels":[{"id":"11"}]}]}}"#);
        assert_eq!(ready.payload.at(&["d", "user", "premium_type"]), Some(&Val::Int(2)));
        assert_eq!(w.account.premium_type, 0, "the real tier is remembered");
        let mut req = Request {
            method: "POST".into(),
            route: "/channels/11/messages".into(),
            body: Some(crate::plugins::testing::j(r#"{"content":"<:ember:111111> <:other:999999>","nonce":"1"}"#)),
            body_changed: false,
        };
        FakeNitro.on_request(&mut w.cx(), &mut req);
        assert!(req.body_changed);
        let c = req.body.unwrap().str_at(&["content"]).unwrap().to_string();
        assert!(c.starts_with("<:ember:111111> [other]("), "{c}");
        // Real Nitro: nothing changes.
        w.account.premium_type = 2;
        let mut req = Request { method: "POST".into(), route: "/channels/11/messages".into(),
            body: Some(crate::plugins::testing::j(r#"{"content":"<:other:999999>"}"#)), body_changed: false };
        FakeNitro.on_request(&mut w.cx(), &mut req);
        assert!(!req.body_changed);
    }
}
