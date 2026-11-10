//! Hide the game you're playing from your status (Vencord: GameActivityToggle).
//! `ashcord toggle game_activity_toggle` flips it live; Ashcord re-sends your status right away.

use super::{Cx, Info, Plugin};
use crate::config::Config;
use crate::val::Val;

pub struct GameActivityToggle;

/// Activity type 0 = "Playing".
pub fn strip_games(payload: &mut Val) -> bool {
    if payload.get("op").and_then(Val::as_i64) != Some(3) {
        return false;
    }
    let Some(list) = payload
        .get_mut("d")
        .and_then(|d| d.get_mut("activities"))
        .and_then(Val::as_list_mut)
    else {
        return false;
    };
    let before = list.len();
    list.retain(|a| a.get("type").and_then(Val::as_i64) != Some(0));
    list.len() != before
}

impl Plugin for GameActivityToggle {
    fn info(&self) -> Info {
        Info {
            id: "game_activity_toggle",
            vencord: "GameActivityToggle",
            about: "While on, your status doesn't show the game you're playing",
        }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugins.game_activity_toggle.enabled
    }
    fn uses_gateway(&self) -> bool {
        true
    }
    fn on_send(&self, _cx: &mut Cx, payload: &mut Val) -> bool {
        strip_games(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::testing::j;
    #[test]
    fn strips_playing_only() {
        let mut p = j(
            r#"{"op":3,"d":{"status":"online","activities":[{"type":0,"name":"CS2"},{"type":2,"name":"Spotify"}]}}"#,
        );
        assert!(strip_games(&mut p));
        assert_eq!(p.at(&["d", "activities"]).unwrap().as_list().len(), 1);
        assert!(!strip_games(&mut p));
        let mut hb = j(r#"{"op":1,"d":5}"#);
        assert!(!strip_games(&mut hb));
    }
}
