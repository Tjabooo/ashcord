//! Start a Discord client that sends its traffic through Ashcord.

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::process::Command;

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// The command for `args` (or the first Discord found), told to use the proxy at `addr`.
pub fn command(args: &[String], addr: &str) -> Result<Command> {
    let url = format!("http://{addr}");
    let flag = format!("--proxy-server={url}");
    let mut cmd = if let Some((prog, rest)) = args.split_first() {
        let mut c = Command::new(prog);
        c.args(rest).arg(&flag);
        c
    } else {
        detect(&flag)?
    };
    // Native (non-Chromium) clients usually honour these instead of --proxy-server.
    for k in ["HTTPS_PROXY", "https_proxy"] {
        cmd.env(k, &url);
    }
    cmd.env("NO_PROXY", "localhost,127.0.0.1,::1");
    Ok(cmd)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn detect(flag: &str) -> Result<Command> {
    for name in ["discord", "Discord", "discord-canary", "discord-ptb"] {
        if let Some(p) = which(name) {
            let mut c = Command::new(p);
            c.arg(flag);
            return Ok(c);
        }
    }
    if which("flatpak").is_some()
        && Command::new("flatpak").args(["info", "com.discordapp.Discord"]).output().is_ok_and(|o| o.status.success())
    {
        let mut c = Command::new("flatpak");
        c.args(["run", "com.discordapp.Discord", flag]);
        return Ok(c);
    }
    bail!("no Discord found on PATH; pass the command: ashcord launch -- /path/to/discord")
}

#[cfg(windows)]
fn detect(flag: &str) -> Result<Command> {
    let local = dirs::data_local_dir().unwrap_or_default();
    for (dir, exe) in [("Discord", "Discord.exe"), ("DiscordCanary", "DiscordCanary.exe"), ("DiscordPTB", "DiscordPTB.exe")] {
        let update = local.join(dir).join("Update.exe");
        if update.is_file() {
            let mut c = Command::new(update);
            c.args(["--processStart", exe, "--process-start-args", flag]);
            return Ok(c);
        }
    }
    let _ = which;
    bail!("no Discord found in %LOCALAPPDATA%; pass the command: ashcord launch -- C:\\path\\to\\Discord.exe")
}

#[cfg(target_os = "macos")]
fn detect(flag: &str) -> Result<Command> {
    for app in ["Discord", "Discord Canary", "Discord PTB"] {
        let p = PathBuf::from(format!("/Applications/{app}.app/Contents/MacOS/{app}"));
        if p.is_file() {
            let mut c = Command::new(p);
            c.arg(flag);
            return Ok(c);
        }
    }
    let _ = which;
    bail!("no Discord found in /Applications; pass the command: ashcord launch -- /path/to/Discord")
}
