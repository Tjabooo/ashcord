//! Keep deleted messages on screen and show what edited messages used to say (Vencord: MessageLogger).
//!
//! Ashcord remembers recent messages. When Discord says one was deleted, the client gets an edit
//! instead, with a "deleted" line on top; edits get their earlier versions underneath. Like Vencord,
//! this only lasts until the client reloads the channel. With `write_log` they're also saved to disk.

use super::{Cx, Dispatch, Info, Plugin, message_route_channel};
use crate::config::Config;
use crate::val::Val;
use lru::LruCache;
use std::io::Write;
use std::num::NonZeroUsize;
use std::sync::Mutex;

struct Entry {
    msg: Val,
    history: Vec<String>,
}

pub struct MessageLogger {
    cache: Mutex<Option<LruCache<u64, Entry>>>,
}

impl Default for MessageLogger {
    fn default() -> Self {
        MessageLogger {
            cache: Mutex::new(None),
        }
    }
}

const DELETED: &str = "-# 🗑 deleted";
const MAX_HISTORY: usize = 5;

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

fn annotate(content: &str, history: &[String]) -> String {
    let mut out = content.to_string();
    for old in history.iter().rev() {
        out.push_str(&format!("\n-# ✏ was: {}", one_line(old, 300)));
    }
    out
}

impl MessageLogger {
    fn with_cache<R>(&self, size: usize, f: impl FnOnce(&mut LruCache<u64, Entry>) -> R) -> R {
        let mut guard = self.cache.lock().unwrap();
        let size = NonZeroUsize::new(size.max(100)).unwrap();
        let cache = guard.get_or_insert_with(|| LruCache::new(size));
        if cache.cap() != size {
            cache.resize(size);
        }
        f(cache)
    }

    fn skip(&self, cx: &Cx, msg: &Val) -> bool {
        let o = &cx.cfg.plugins.message_logger;
        (o.ignore_self && cx.account.is_me(msg.id_at(&["author", "id"])))
            || (o.ignore_bots && msg.at(&["author", "bot"]).and_then(Val::as_bool) == Some(true))
    }

    fn remember(&self, cx: &Cx, msg: &Val) {
        let Some(id) = msg.id_at(&["id"]) else { return };
        self.with_cache(cx.cfg.plugins.message_logger.cache_size, |c| {
            match c.get_mut(&id) {
                Some(e) => e.msg = msg.clone(),
                None => {
                    c.put(
                        id,
                        Entry {
                            msg: msg.clone(),
                            history: Vec::new(),
                        },
                    );
                }
            }
        });
    }

    fn log(&self, cx: &Cx, kind: &str, msg: &Val, old: Option<&str>) {
        if !cx.cfg.plugins.message_logger.write_log {
            return;
        }
        let line = serde_json::json!({
            "time": humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string(),
            "kind": kind,
            "channel": msg.id_at(&["channel_id"]).map(|v| v.to_string()),
            "guild": msg.id_at(&["guild_id"]).and_then(|g| cx.account.guild_names.get(&g).cloned()),
            "author": msg.str_at(&["author", "global_name"]).or_else(|| msg.str_at(&["author", "username"])),
            "content": msg.str_at(&["content"]),
            "was": old,
            "attachments": msg.get("attachments").map(Val::as_list).unwrap_or_default().iter()
                .filter_map(|a| a.str_at(&["url"])).collect::<Vec<_>>(),
        });
        let path = cx.paths.message_log();
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            writeln!(f, "{line}")
        };
        if let Err(e) = write() {
            tracing::warn!("message log: {e}");
        }
    }

    fn deleted(&self, cx: &Cx, id: u64) -> Option<Val> {
        let size = cx.cfg.plugins.message_logger.cache_size;
        let (msg, history) = self.with_cache(size, |c| c.pop(&id).map(|e| (e.msg, e.history)))?;
        if self.skip(cx, &msg) {
            return None;
        }
        self.log(cx, "deleted", &msg, None);
        let mut shown = msg.clone();
        let text = msg.str_at(&["content"]).unwrap_or("");
        let body = if text.is_empty() {
            DELETED.to_string()
        } else {
            format!("{DELETED}\n{text}")
        };
        shown.set(
            "content",
            msg.get("content")
                .map(|c| c.str_like(&annotate(&body, &history)))
                .unwrap_or(Val::Str(body)),
        );
        Some(shown)
    }
}

impl Plugin for MessageLogger {
    fn info(&self) -> Info {
        Info {
            id: "message_logger",
            vencord: "MessageLogger",
            about: "Keep deleted messages visible and show edit history",
        }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugins.message_logger.enabled
    }
    fn uses_gateway(&self) -> bool {
        true
    }
    fn wants_response(&self, method: &str, route: &str) -> bool {
        method == "GET" && route.ends_with("/messages") && message_route_channel(route).is_some()
    }
    fn on_response(&self, cx: &mut Cx, method: &str, route: &str, body: &Val) {
        if self.wants_response(method, route) {
            for m in body.as_list() {
                self.remember(cx, m);
            }
        }
    }
    fn on_dispatch(&self, cx: &mut Cx, d: &mut Dispatch) {
        let Some(t) = d.event().map(str::to_string) else {
            return;
        };
        match t.as_str() {
            "MESSAGE_CREATE" => {
                if let Some(m) = d.payload.get("d") {
                    self.remember(cx, m);
                }
            }
            "MESSAGE_UPDATE" => {
                let Some(m) = d.payload.get("d").cloned() else {
                    return;
                };
                let Some(id) = m.id_at(&["id"]) else { return };
                let Some(new) = m.str_at(&["content"]).map(str::to_string) else {
                    return;
                };
                let o = cx.cfg.plugins.message_logger.clone();
                let size = o.cache_size;
                let skip = self.skip(cx, &m);
                let edit = self.with_cache(size, |c| {
                    let e = c.get_mut(&id)?;
                    let old = e.msg.str_at(&["content"]).unwrap_or("").to_string();
                    e.msg = m.clone();
                    if old == new || skip {
                        return None;
                    }
                    e.history.push(old.clone());
                    if e.history.len() > MAX_HISTORY {
                        e.history.remove(0);
                    }
                    Some((old, e.history.clone()))
                });
                let Some((old, history)) = edit else { return };
                self.log(cx, "edited", &m, Some(&old));
                if o.show_edits
                    && let Some(data) = d.data()
                {
                    let shown = annotate(&new, &history);
                    let v = data
                        .get("content")
                        .map(|c| c.str_like(&shown))
                        .unwrap_or(Val::Str(shown));
                    data.set("content", v);
                    d.changed = true;
                }
            }
            "MESSAGE_DELETE" => {
                let Some(id) = d.payload.id_at(&["d", "id"]) else {
                    return;
                };
                if let Some(shown) = self.deleted(cx, id) {
                    d.rewrite("MESSAGE_UPDATE", shown);
                }
            }
            "MESSAGE_DELETE_BULK" => {
                let ids: Vec<Val> = d
                    .payload
                    .at(&["d", "ids"])
                    .map(|l| l.as_list().to_vec())
                    .unwrap_or_default();
                let template = d.payload.clone();
                let mut shown = Vec::new();
                let mut gone = Vec::new();
                for v in ids {
                    match v.snowflake().and_then(|id| self.deleted(cx, id)) {
                        Some(m) => shown.push(m),
                        None => gone.push(v),
                    }
                }
                let mut shown = shown.into_iter();
                let Some(first) = shown.next() else { return };
                let event = |t: &str, data: Val| {
                    let mut p = template.clone();
                    let tv = p
                        .get("t")
                        .map(|v| v.str_like(t))
                        .unwrap_or(Val::Str(t.into()));
                    p.set("t", tv);
                    p.set("d", data);
                    p
                };
                d.rewrite("MESSAGE_UPDATE", first);
                d.extra.extend(shown.map(|m| event("MESSAGE_UPDATE", m)));
                // Messages Ashcord didn't have (or that you deleted yourself) are still deleted.
                if !gone.is_empty() {
                    let mut data = template.get("d").cloned().unwrap_or_else(Val::map);
                    data.set("ids", Val::List(gone));
                    d.extra.push(event("MESSAGE_DELETE_BULK", data));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::testing::World;

    fn world() -> World {
        let mut w = World::new();
        w.cfg.plugins.message_logger.enabled = true;
        w.cfg.plugins.message_logger.write_log = false;
        w.account.user_id = Some(1);
        w
    }

    #[test]
    fn delete_becomes_marked_edit() {
        let mut w = world();
        let p = MessageLogger::default();
        w.dispatch(&p, r#"{"op":0,"s":5,"t":"MESSAGE_CREATE","d":{"id":"50","channel_id":"9","content":"hej","author":{"id":"2"}}}"#);
        let d = w.dispatch(
            &p,
            r#"{"op":0,"s":6,"t":"MESSAGE_DELETE","d":{"id":"50","channel_id":"9"}}"#,
        );
        assert!(d.changed);
        assert_eq!(d.event(), Some("MESSAGE_UPDATE"));
        assert_eq!(
            d.payload.str_at(&["d", "content"]),
            Some("-# 🗑 deleted\nhej")
        );
        assert_eq!(d.payload.get("s").and_then(Val::as_i64), Some(6));
        // Unknown messages are left alone.
        let d = w.dispatch(
            &p,
            r#"{"op":0,"t":"MESSAGE_DELETE","d":{"id":"51","channel_id":"9"}}"#,
        );
        assert!(!d.changed);
    }

    #[test]
    fn edits_show_history_and_own_messages_are_ignored() {
        let mut w = world();
        let p = MessageLogger::default();
        w.dispatch(&p, r#"{"op":0,"t":"MESSAGE_CREATE","d":{"id":"60","channel_id":"9","content":"v1","author":{"id":"2"}}}"#);
        let d = w.dispatch(&p, r#"{"op":0,"t":"MESSAGE_UPDATE","d":{"id":"60","channel_id":"9","content":"v2","author":{"id":"2"}}}"#);
        assert_eq!(
            d.payload.str_at(&["d", "content"]),
            Some("v2\n-# ✏ was: v1")
        );
        // Embed-only updates (no content) don't count as edits.
        let d = w.dispatch(
            &p,
            r#"{"op":0,"t":"MESSAGE_UPDATE","d":{"id":"60","channel_id":"9","embeds":[]}}"#,
        );
        assert!(!d.changed);

        w.dispatch(&p, r#"{"op":0,"t":"MESSAGE_CREATE","d":{"id":"61","channel_id":"9","content":"mine","author":{"id":"1"}}}"#);
        let d = w.dispatch(
            &p,
            r#"{"op":0,"t":"MESSAGE_DELETE","d":{"id":"61","channel_id":"9"}}"#,
        );
        assert!(!d.changed, "own deletions stay deleted with ignore_self");
    }

    #[test]
    fn bulk_delete_keeps_every_known_message() {
        let mut w = world();
        let p = MessageLogger::default();
        for id in ["70", "71"] {
            w.dispatch(&p, &format!(r#"{{"op":0,"t":"MESSAGE_CREATE","d":{{"id":"{id}","channel_id":"9","content":"m{id}","author":{{"id":"2"}}}}}}"#));
        }
        let d = w.dispatch(
            &p,
            r#"{"op":0,"t":"MESSAGE_DELETE_BULK","d":{"ids":["70","71","72"],"channel_id":"9"}}"#,
        );
        assert_eq!(d.event(), Some("MESSAGE_UPDATE"));
        assert_eq!(d.extra.len(), 2);
        assert_eq!(
            d.extra[0].str_at(&["d", "content"]),
            Some("-# 🗑 deleted\nm71")
        );
        assert_eq!(d.extra[1].str_at(&["t"]), Some("MESSAGE_DELETE_BULK"));
        assert_eq!(d.extra[1].at(&["d", "ids"]).unwrap().as_list().len(), 1);
    }

    #[test]
    fn learns_from_fetched_history() {
        let mut w = world();
        let p = MessageLogger::default();
        let body = crate::plugins::testing::j(
            r#"[{"id":"80","channel_id":"9","content":"old","author":{"id":"2"}}]"#,
        );
        p.on_response(&mut w.cx(), "GET", "/channels/9/messages", &body);
        let d = w.dispatch(
            &p,
            r#"{"op":0,"t":"MESSAGE_DELETE","d":{"id":"80","channel_id":"9"}}"#,
        );
        assert!(d.changed);
    }
}
