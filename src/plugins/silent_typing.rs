//! Don't tell others you're typing (Vencord: SilentTyping).

use super::{Cx, Info, Plugin, Request};
use crate::config::Config;

pub struct SilentTyping;

impl Plugin for SilentTyping {
    fn info(&self) -> Info {
        Info { id: "silent_typing", vencord: "SilentTyping", about: "Hide your typing indicator from others" }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugins.silent_typing.enabled
    }
    fn on_request(&self, _cx: &mut Cx, req: &mut Request) -> Option<u16> {
        let seg: Vec<&str> = req.route.trim_matches('/').split('/').collect();
        (req.method == "POST" && matches!(seg.as_slice(), ["channels", _, "typing"])).then_some(204)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::testing::World;
    #[test]
    fn blocks_typing() {
        let mut w = World::new();
        let mut r = Request { method: "POST".into(), route: "/channels/9/typing".into(), body: None, body_changed: false };
        assert_eq!(SilentTyping.on_request(&mut w.cx(), &mut r), Some(204));
        r.route = "/channels/9/messages".into();
        assert_eq!(SilentTyping.on_request(&mut w.cx(), &mut r), None);
    }
}
