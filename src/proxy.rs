//! The local HTTP proxy. Clients send `CONNECT host:443`; Discord's API and gateway hosts are opened up
//! with a certificate from the local CA, everything else (CDN, media, voice, other sites) is tunnelled
//! through byte for byte.

use crate::app::App;
use crate::rest::{self, Body, Upstream, empty, full};
use crate::gateway;
use anyhow::Result;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

pub async fn bind(addr: &str) -> Result<TcpListener> {
    Ok(TcpListener::bind(addr).await?)
}

pub async fn serve(app: Arc<App>, listener: TcpListener) -> Result<()> {
    loop {
        let (sock, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            tracing::warn!("refused a connection from {peer}: Ashcord only serves this computer");
            continue;
        }
        sock.set_nodelay(true).ok();
        let app = app.clone();
        tokio::spawn(async move {
            let svc = service_fn(move |req| front(app.clone(), req, peer));
            if let Err(e) = http1::Builder::new().serve_connection(TokioIo::new(sock), svc).with_upgrades().await {
                tracing::debug!("proxy connection: {e}");
            }
        });
    }
}

async fn front(app: Arc<App>, req: Request<Incoming>, _peer: SocketAddr) -> Result<Response<Body>, Infallible> {
    if req.method() != Method::CONNECT {
        let mut r = Response::new(full("Ashcord is an HTTPS proxy: use it as the client's proxy (CONNECT).\n"));
        *r.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
        return Ok(r);
    }
    let Some(auth) = req.uri().authority().cloned() else {
        let mut r = Response::new(empty());
        *r.status_mut() = StatusCode::BAD_REQUEST;
        return Ok(r);
    };
    let host = auth.host().to_string();
    let port = auth.port_u16().unwrap_or(443);
    let intercept = port == 443 && app.cfg().intercepts(&host);
    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => return tracing::debug!("CONNECT upgrade failed: {e}"),
        };
        let io = TokioIo::new(upgraded);
        let result = if intercept { mitm(app, io, host.clone()).await } else { tunnel(io, &host, port).await };
        if let Err(e) = result {
            tracing::debug!("{host}:{port}: {e:#}");
        }
    });
    Ok(Response::new(empty()))
}

async fn tunnel(mut io: TokioIo<hyper::upgrade::Upgraded>, host: &str, port: u16) -> Result<()> {
    let mut up = TcpStream::connect((host, port)).await?;
    up.set_nodelay(true).ok();
    tokio::io::copy_bidirectional(&mut io, &mut up).await?;
    Ok(())
}

async fn mitm(app: Arc<App>, io: TokioIo<hyper::upgrade::Upgraded>, host: String) -> Result<()> {
    let tls = tokio_rustls::TlsAcceptor::from(app.server_tls.clone()).accept(io).await?;
    let upstream = Arc::new(Upstream::new(host.clone(), app.upstream_tls.clone()));
    let svc = service_fn(move |req: Request<Incoming>| {
        let (app, upstream, host) = (app.clone(), upstream.clone(), host.clone());
        async move {
            if gateway::is_upgrade(&req) {
                Ok::<_, Infallible>(gateway::upgrade(app, req, host).await.unwrap_or_else(|e| {
                    tracing::warn!("gateway: {e:#}");
                    let mut r = Response::new(full("Ashcord could not reach the gateway"));
                    *r.status_mut() = StatusCode::BAD_GATEWAY;
                    r
                }))
            } else {
                rest::handle(app, upstream, req).await
            }
        }
    });
    http1::Builder::new().serve_connection(TokioIo::new(tls), svc).with_upgrades().await?;
    Ok(())
}
