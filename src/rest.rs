//! HTTPS requests to Discord's API (everything that isn't the gateway websocket).

use crate::app::App;
use crate::plugins::{Request as PReq, route_of};
use crate::val::Val;
use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use std::convert::Infallible;
use std::io::Read;
use std::sync::Arc;
use tokio::net::TcpStream;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = BoxBody<Bytes, BoxError>;

pub fn full(b: impl Into<Bytes>) -> Body {
    Full::new(b.into()).map_err(|e| match e {}).boxed()
}

pub fn empty() -> Body {
    Empty::new().map_err(|e| match e {}).boxed()
}

pub fn incoming(b: Incoming) -> Body {
    b.map_err(|e| Box::new(e) as BoxError).boxed()
}

const HOP_BY_HOP: &[&str] =
    &["connection", "keep-alive", "proxy-connection", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade"];

fn strip_hop_by_hop(h: &mut HeaderMap) {
    for name in HOP_BY_HOP {
        h.remove(*name);
    }
}

fn is_json(h: &HeaderMap) -> bool {
    h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/json"))
}

/// One kept-alive HTTPS connection to the real host, opened on first use and re-opened when it drops.
pub struct Upstream {
    host: String,
    tls: Arc<rustls::ClientConfig>,
    sender: tokio::sync::Mutex<Option<SendRequest<Body>>>,
}

impl Upstream {
    pub fn new(host: String, tls: Arc<rustls::ClientConfig>) -> Upstream {
        Upstream { host, tls, sender: tokio::sync::Mutex::new(None) }
    }

    async fn connect(&self) -> Result<SendRequest<Body>> {
        let tcp = TcpStream::connect((self.host.as_str(), 443)).await.with_context(|| format!("connecting to {}", self.host))?;
        tcp.set_nodelay(true).ok();
        let name = ServerName::try_from(self.host.clone())?;
        let tls = tokio_rustls::TlsConnector::from(self.tls.clone()).connect(name, tcp).await?;
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!("upstream connection closed: {e}");
            }
        });
        Ok(sender)
    }

    async fn send(&self, req: Request<Body>) -> Result<Response<Incoming>> {
        let mut guard = self.sender.lock().await;
        if guard.as_ref().is_none_or(|s| s.is_closed()) {
            *guard = Some(self.connect().await?);
        }
        let s = guard.as_mut().unwrap();
        if s.ready().await.is_err() {
            *guard = Some(self.connect().await?);
        }
        Ok(guard.as_mut().unwrap().send_request(req).await?)
    }
}

pub async fn handle(app: Arc<App>, up: Arc<Upstream>, req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let what = format!("{} {}", req.method(), req.uri().path());
    match forward(app, up, req).await {
        Ok(r) => Ok(r),
        Err(e) => {
            tracing::warn!("{what}: {e:#}");
            let mut r = Response::new(full("Ashcord could not reach Discord"));
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            Ok(r)
        }
    }
}

async fn forward(app: Arc<App>, up: Arc<Upstream>, req: Request<Incoming>) -> Result<Response<Body>> {
    let (mut parts, body) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let route = route_of(parts.uri.path());

    let mut kept: Option<Bytes> = None;
    let mut stream = Some(body);
    let mut json = None;
    if app.wants_request(&method, &route) && is_json(&parts.headers) {
        let b = stream.take().unwrap().collect().await?.to_bytes();
        json = serde_json::from_slice(&b).ok().map(Val::from_json);
        kept = Some(b);
    }

    let mut preq = PReq { method, route, body: json, body_changed: false };
    if let Some(status) = app.on_request(&mut preq) {
        let mut r = Response::new(empty());
        *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::NO_CONTENT);
        return Ok(r);
    }

    let out: Body = match (preq.body_changed, preq.body, kept, stream) {
        (true, Some(v), _, _) => {
            let b = serde_json::to_vec(&v.to_json())?;
            parts.headers.insert(header::CONTENT_LENGTH, HeaderValue::from(b.len()));
            full(b)
        }
        (_, _, Some(b), _) => full(b),
        (_, _, None, Some(s)) => incoming(s),
        _ => empty(),
    };

    strip_hop_by_hop(&mut parts.headers);
    let pq = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    parts.uri = pq.parse()?;
    let wants_response = app.wants_response(&preq.method, &preq.route);
    if wants_response {
        parts.headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
    }

    let resp = up.send(Request::from_parts(parts, out)).await?;
    let (mut rp, rb) = resp.into_parts();
    strip_hop_by_hop(&mut rp.headers);

    if wants_response && rp.status.is_success() && is_json(&rp.headers) {
        let raw = rb.collect().await?.to_bytes();
        let gz = rp.headers.get(header::CONTENT_ENCODING).is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"gzip"));
        let plain = if gz {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out)?;
            Bytes::from(out)
        } else {
            raw
        };
        if let Ok(j) = serde_json::from_slice(&plain) {
            app.on_response(&preq.method, &preq.route, &Val::from_json(j));
        }
        rp.headers.remove(header::CONTENT_ENCODING);
        rp.headers.insert(header::CONTENT_LENGTH, HeaderValue::from(plain.len()));
        return Ok(Response::from_parts(rp, full(plain)));
    }
    Ok(Response::from_parts(rp, incoming(rb)))
}
