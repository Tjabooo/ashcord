//! Blocks Discord's analytics and crash reporting (Vencord: NoTrack).

use super::{Cx, Info, Plugin, Request};
use crate::config::Config;

pub struct NoTrack;

fn is_tracking(method: &str, route: &str) -> bool {
    let r = route.trim_end_matches('/');
    (method == "POST" && matches!(r, "/science" | "/track" | "/metrics" | "/metrics/v2"))
        || r.starts_with("/error-reporting-proxy")
}

impl Plugin for NoTrack {
    fn info(&self) -> Info {
        Info {
            id: "no_track",
            vencord: "NoTrack",
            about: "Block Discord's analytics and crash reports",
        }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugins.no_track.enabled
    }
    fn on_request(&self, _cx: &mut Cx, req: &mut Request) -> Option<u16> {
        is_tracking(&req.method, &req.route).then_some(204)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_tracking_only() {
        assert!(is_tracking("POST", "/science"));
        assert!(is_tracking("POST", "/metrics/v2"));
        assert!(is_tracking("POST", "/error-reporting-proxy/web"));
        assert!(!is_tracking("GET", "/science"));
        assert!(!is_tracking("POST", "/channels/1/messages"));
    }
}
