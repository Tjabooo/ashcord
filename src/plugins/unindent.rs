//! Remove the shared indentation from code blocks you send (Vencord: Unindent).

use super::{Cx, Info, Plugin, Request, message_route_channel};
use crate::config::Config;
use crate::val::Val;

pub struct Unindent;

fn dedent_block(block: &str) -> String {
    // block = "lang\ncode..." (the text between two ``` fences).
    let Some((first, body)) = block.split_once('\n') else { return block.to_string() };
    let indent = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    if indent == 0 {
        return block.to_string();
    }
    let lines: Vec<&str> = body.split('\n').map(|l| if l.len() >= indent { &l[indent..] } else { l.trim_start() }).collect();
    format!("{first}\n{}", lines.join("\n"))
}

pub fn unindent(content: &str) -> String {
    let parts: Vec<&str> = content.split("```").collect();
    if parts.len() < 3 {
        return content.to_string();
    }
    let last = parts.len() - 1;
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| if i % 2 == 1 && i < last { dedent_block(p) } else { p.to_string() })
        .collect::<Vec<_>>()
        .join("```")
}

impl Plugin for Unindent {
    fn info(&self) -> Info {
        Info { id: "unindent", vencord: "Unindent", about: "Strip shared indentation from code blocks you send" }
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.plugins.unindent.enabled
    }
    fn wants_request(&self, method: &str, route: &str) -> bool {
        matches!(method, "POST" | "PATCH") && message_route_channel(route).is_some()
    }
    fn on_request(&self, _cx: &mut Cx, req: &mut Request) -> Option<u16> {
        if !self.wants_request(&req.method, &req.route) {
            return None;
        }
        let body = req.body.as_mut()?;
        let text = body.str_at(&["content"])?;
        let fixed = unindent(text);
        if fixed != text {
            body.set("content", Val::Str(fixed));
            req.body_changed = true;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dedents_code_blocks_only() {
        let s = "look:\n```rs\n    fn a() {\n        b();\n    }\n```\n    not code";
        assert_eq!(unindent(s), "look:\n```rs\nfn a() {\n    b();\n}\n```\n    not code");
        assert_eq!(unindent("no ``` fence closed"), "no ``` fence closed");
        assert_eq!(unindent("```\nflat\n```"), "```\nflat\n```");
    }
}
