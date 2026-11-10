//! Ashcord: Vencord-style plugins for Discord, working at the network layer so they don't depend on
//! the client's code. Built for Discord's native Rust client and working with today's client as well.

mod app;
mod ca;
mod codec;
mod config;
mod etf;
mod gateway;
mod launch;
mod notify;
mod plugins;
mod proxy;
mod rest;
mod selftest;
mod state;
mod trust;
mod val;
mod vencord;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::{Config, Paths};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "ashcord",
    version,
    about = "Vencord-style plugins for Discord, through a local proxy"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the proxy (the default). Point Discord at it with `--proxy-server=http://127.0.0.1:18463`.
    Run,
    /// Run the proxy and start Discord through it (quit Discord first). Optional: the client command.
    Launch {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// First-time setup: write the config, make the local CA and trust it, import Vencord settings.
    Setup,
    /// Trust the local CA in the certificate stores Discord uses.
    Trust,
    /// Remove the local CA from those stores.
    Untrust,
    /// List plugins and whether they're on.
    Plugins,
    /// Switch a plugin on.
    Enable { name: String },
    /// Switch a plugin off.
    Disable { name: String },
    /// Flip a plugin (e.g. `ashcord toggle game_activity_toggle`).
    Toggle { name: String },
    /// Copy plugin choices from Vencord / Vesktop / Equicord.
    ImportVencord { path: Option<std::path::PathBuf> },
    /// Show deleted and edited messages saved by the message logger.
    Log {
        #[arg(short, default_value_t = 20)]
        n: usize,
    },
    /// Check everything works against the real Discord (no login needed).
    Selftest,
    /// Print where config, CA and logs live.
    Paths,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ASHCORD_LOG")
                .unwrap_or_else(|_| "ashcord=info".into()),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let paths = Paths::resolve();
    let rt = tokio::runtime::Runtime::new()?;
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => rt.block_on(run(paths, None)),
        Cmd::Launch { command } => rt.block_on(run(paths, Some(command))),
        Cmd::Setup => setup(&paths),
        Cmd::Trust => {
            let ca = ca::Authority::load_or_create(&paths.data)?;
            trust::install(&ca.ca_pem_path)
        }
        Cmd::Untrust => trust::uninstall(),
        Cmd::Plugins => {
            let cfg = Config::load_or_init(&paths.config)?;
            for p in plugins::all() {
                let i = p.info();
                let on = if p.enabled(&cfg) { "on " } else { "off" };
                println!("{on}  {:<22} {}  (Vencord: {})", i.id, i.about, i.vencord);
            }
            println!("\nConfig: {}", paths.config.display());
            Ok(())
        }
        Cmd::Enable { name } => switch(&paths, &name, Some(true)),
        Cmd::Disable { name } => switch(&paths, &name, Some(false)),
        Cmd::Toggle { name } => switch(&paths, &name, None),
        Cmd::ImportVencord { path } => import(&paths, path),
        Cmd::Log { n } => show_log(&paths, n),
        Cmd::Selftest => rt.block_on(selftest::run(paths)),
        Cmd::Paths => {
            println!("config  {}", paths.config.display());
            println!("CA      {}", paths.data.join("ca.pem").display());
            println!("log     {}", paths.message_log().display());
            Ok(())
        }
    }
}

async fn run(paths: Paths, launch: Option<Vec<String>>) -> Result<()> {
    let cfg = Config::load_or_init(&paths.config)?;
    let listen = cfg.listen.clone();
    let app = app::App::new(paths, cfg, false)?;
    app.spawn_reloader();
    let listener = proxy::bind(&listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    tracing::info!("Ashcord listening on {listen} (plugins: {})", {
        let cfg = app.cfg();
        app.plugins
            .iter()
            .filter(|p| p.enabled(&cfg))
            .map(|p| p.info().id)
            .collect::<Vec<_>>()
            .join(", ")
    });
    let server = tokio::spawn(proxy::serve(app.clone(), listener));

    if let Some(args) = launch {
        let cmd = launch::command(&args, &listen)?;
        tracing::info!("starting {:?}", cmd.get_program());
        let started = Instant::now();
        let mut child = tokio::process::Command::from(cmd)
            .spawn()
            .context("starting Discord")?;
        tokio::select! {
            status = child.wait() => {
                let status = status?;
                // Launchers (Update.exe, flatpak) hand off and exit at once: keep serving then.
                if started.elapsed() < Duration::from_secs(15) {
                    tracing::info!("launcher exited ({status}); still serving, Ctrl+C to stop");
                    let _ = tokio::signal::ctrl_c().await;
                } else {
                    tracing::info!("Discord exited ({status})");
                }
            }
            _ = tokio::signal::ctrl_c() => {}
        }
        return Ok(());
    }
    tokio::select! {
        r = server => r??,
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}

fn setup(paths: &Paths) -> Result<()> {
    let mut cfg = Config::load_or_init(&paths.config)?;
    println!("config: {}", paths.config.display());
    if let Some(v) = vencord::find() {
        let changes = vencord::import(&v, &mut cfg)?;
        cfg.save(&paths.config)?;
        println!(
            "imported from {}: {}",
            v.display(),
            if changes.is_empty() {
                "nothing new".into()
            } else {
                changes.join(", ")
            }
        );
    }
    let ca = ca::Authority::load_or_create(&paths.data)?;
    println!(
        "local CA: {} (only valid for {})",
        ca.ca_pem_path.display(),
        ca::CONSTRAINED_TO.join(", ")
    );
    trust::install(&ca.ca_pem_path)?;
    println!("\nDone. Quit Discord fully, then start it with `ashcord launch`.");
    Ok(())
}

fn switch(paths: &Paths, name: &str, to: Option<bool>) -> Result<()> {
    let mut cfg = Config::load_or_init(&paths.config)?;
    let now = cfg
        .plugin_enabled(name)
        .with_context(|| format!("no plugin named {name} (see `ashcord plugins`)"))?;
    let on = to.unwrap_or(!now);
    cfg.set_plugin(name, on)?;
    cfg.save(&paths.config)?;
    println!("{name}: {}", if on { "on" } else { "off" });
    let gateway = plugins::all()
        .iter()
        .any(|p| p.info().id == name && p.uses_gateway());
    if gateway && on && !now {
        println!(
            "(gateway plugin: takes effect when Discord reconnects; Ctrl+R in Discord does that)"
        );
    }
    Ok(())
}

fn import(paths: &Paths, path: Option<std::path::PathBuf>) -> Result<()> {
    let path = path
        .or_else(vencord::find)
        .context("no Vencord settings found; pass the path to settings.json")?;
    let mut cfg = Config::load_or_init(&paths.config)?;
    let changes = vencord::import(&path, &mut cfg)?;
    cfg.save(&paths.config)?;
    println!("from {}:", path.display());
    if changes.is_empty() {
        println!("  nothing to change");
    }
    for c in changes {
        println!("  {c}");
    }
    Ok(())
}

fn show_log(paths: &Paths, n: usize) -> Result<()> {
    let p = paths.message_log();
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        println!("nothing logged yet ({})", p.display());
    }
    for l in &lines[lines.len().saturating_sub(n)..] {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let place = if s("guild").is_empty() {
            format!("channel {}", s("channel"))
        } else {
            s("guild")
        };
        match s("kind").as_str() {
            "edited" => println!(
                "{}  ✏ {} in {place}: {:?} → {:?}",
                s("time"),
                s("author"),
                s("was"),
                s("content")
            ),
            _ => println!(
                "{}  🗑 {} in {place}: {:?}",
                s("time"),
                s("author"),
                s("content")
            ),
        }
    }
    Ok(())
}
