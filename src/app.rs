//! Shared state for the whole proxy: config (reloaded live), account knowledge, plugins, TLS.

use crate::ca::Authority;
use crate::config::{Config, Paths};
use crate::plugins::{self, Cx, Dispatch, Plugin, Request};
use crate::state::Account;
use crate::val::Val;
use anyhow::Result;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};
use tokio::sync::watch;

pub struct App {
    pub paths: Paths,
    cfg: RwLock<Arc<Config>>,
    pub account: Mutex<Account>,
    pub plugins: Vec<Box<dyn Plugin>>,
    pub ca: Arc<Authority>,
    pub server_tls: Arc<rustls::ServerConfig>,
    pub upstream_tls: Arc<rustls::ClientConfig>,
    /// Bumped on every config reload.
    pub reloads: watch::Sender<u64>,
    /// Decode (and re-encode) the gateway even when no plugin needs it (self-test).
    pub force_decode: bool,
    pub stats: Stats,
}

#[derive(Default)]
pub struct Stats {
    pub decoded: std::sync::atomic::AtomicU64,
    pub undecodable: std::sync::atomic::AtomicU64,
}

pub fn upstream_tls(extra_root: Option<rustls::pki_types::CertificateDer<'static>>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(r) = extra_root {
        let _ = roots.add(r);
    }
    let mut c = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(c)
}

impl App {
    pub fn new(paths: Paths, cfg: Config, force_decode: bool) -> Result<Arc<App>> {
        let ca = Arc::new(Authority::load_or_create(&paths.data)?);
        let server_tls = ca.server_config();
        Ok(Arc::new(App {
            paths,
            cfg: RwLock::new(Arc::new(cfg)),
            account: Mutex::new(Account::default()),
            plugins: plugins::all(),
            ca,
            server_tls,
            upstream_tls: upstream_tls(None),
            reloads: watch::channel(0).0,
            force_decode,
            stats: Stats::default(),
        }))
    }

    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.read().unwrap().clone()
    }

    /// Re-read config.toml whenever it changes (checked every 2 s).
    pub fn spawn_reloader(self: &Arc<Self>) {
        let app = self.clone();
        tokio::spawn(async move {
            let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            let mut last: Option<SystemTime> = mtime(&app.paths.config);
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let now = mtime(&app.paths.config);
                if now == last {
                    continue;
                }
                last = now;
                match Config::load(&app.paths.config) {
                    Ok(c) => {
                        *app.cfg.write().unwrap() = Arc::new(c);
                        app.reloads.send_modify(|n| *n += 1);
                        tracing::info!("config reloaded");
                    }
                    Err(e) => tracing::warn!("config not reloaded: {e:#}"),
                }
            }
        });
    }

    fn active(&self, cfg: &Config) -> impl Iterator<Item = &dyn Plugin> {
        self.plugins.iter().map(|p| p.as_ref()).filter(move |p| p.enabled(cfg))
    }

    pub fn gateway_active(&self) -> bool {
        let cfg = self.cfg();
        self.force_decode || self.active(&cfg).any(|p| p.uses_gateway())
    }

    pub fn wants_request(&self, method: &str, route: &str) -> bool {
        let cfg = self.cfg();
        self.active(&cfg).any(|p| p.wants_request(method, route))
    }

    pub fn wants_response(&self, method: &str, route: &str) -> bool {
        let cfg = self.cfg();
        self.active(&cfg).any(|p| p.wants_response(method, route))
    }

    pub fn on_request(&self, req: &mut Request) -> Option<u16> {
        let cfg = self.cfg();
        let mut account = self.account.lock().unwrap();
        account.record_request(&req.method, &req.route, req.body.as_ref());
        let mut cx = Cx { cfg: &cfg, account: &mut account, paths: &self.paths };
        for p in self.active(&cfg) {
            if let Some(status) = p.on_request(&mut cx, req) {
                tracing::debug!("{}: answered {} {} locally ({status})", p.info().id, req.method, req.route);
                return Some(status);
            }
        }
        None
    }

    pub fn on_response(&self, method: &str, route: &str, body: &Val) {
        let cfg = self.cfg();
        let mut account = self.account.lock().unwrap();
        let mut cx = Cx { cfg: &cfg, account: &mut account, paths: &self.paths };
        for p in self.active(&cfg) {
            p.on_response(&mut cx, method, route, body);
        }
    }

    pub fn on_dispatch(&self, payload: Val) -> Dispatch {
        let cfg = self.cfg();
        let mut account = self.account.lock().unwrap();
        if payload.get("op").and_then(Val::as_i64) == Some(0)
            && let (Some(t), Some(d)) = (payload.str_at(&["t"]), payload.get("d"))
        {
            account.track(t, d);
        }
        let mut d = Dispatch { payload, extra: Vec::new(), changed: false, drop: false };
        let mut cx = Cx { cfg: &cfg, account: &mut account, paths: &self.paths };
        for p in self.active(&cfg) {
            p.on_dispatch(&mut cx, &mut d);
            if d.drop {
                break;
            }
        }
        d
    }

    pub fn on_send(&self, payload: &mut Val) -> bool {
        let cfg = self.cfg();
        let mut account = self.account.lock().unwrap();
        let mut cx = Cx { cfg: &cfg, account: &mut account, paths: &self.paths };
        let mut changed = false;
        for p in self.active(&cfg) {
            changed |= p.on_send(&mut cx, payload);
        }
        changed
    }
}
