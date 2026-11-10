//! The gateway websocket: Ashcord sits between client and Discord, decodes each payload, lets plugins
//! change it and re-encodes it with the client's own encoding and compression.

use crate::app::App;
use crate::codec::{Codec, Compression, Deflater, Encoding, Inflater};
use crate::rest::{Body, empty};
use crate::val::Val;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::{Bytes, Message};

pub fn is_upgrade(req: &Request<Incoming>) -> bool {
    req.headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
}

/// Headers worth passing on to Discord when opening the real gateway connection.
const FORWARD: &[header::HeaderName] = &[
    header::USER_AGENT,
    header::ORIGIN,
    header::ACCEPT_LANGUAGE,
    header::COOKIE,
];

pub async fn upgrade(
    app: Arc<App>,
    req: Request<Incoming>,
    host: String,
) -> Result<Response<Body>> {
    let key = req
        .headers()
        .get(header::SEC_WEBSOCKET_KEY)
        .context("websocket upgrade without a key")?;
    let accept = derive_accept_key(key.as_bytes());
    let pq = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let codec = Codec::from_query(req.uri().query().unwrap_or(""));

    let mut up_req = format!("wss://{host}{pq}").into_client_request()?;
    for h in FORWARD {
        if let Some(v) = req.headers().get(h) {
            up_req.headers_mut().insert(h.clone(), v.clone());
        }
    }
    let tcp = TcpStream::connect((host.as_str(), 443)).await?;
    tcp.set_nodelay(true).ok();
    let tls = tokio_rustls::TlsConnector::from(app.upstream_tls.clone())
        .connect(ServerName::try_from(host.clone())?, tcp)
        .await?;
    let (upstream, _) = tokio_tungstenite::client_async(up_req, tls)
        .await
        .context("gateway handshake")?;

    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => return tracing::warn!("client websocket upgrade failed: {e}"),
        };
        let client =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
        let result = match codec {
            Ok(c) if app.gateway_active() => bridge(app, client, upstream, c).await,
            Ok(_) => passthrough(client, upstream).await,
            Err(e) => {
                tracing::warn!("{e:#}; passing the gateway through untouched");
                passthrough(client, upstream).await
            }
        };
        if let Err(e) = result {
            tracing::debug!("gateway connection ended: {e:#}");
        }
    });

    let mut r = Response::new(empty());
    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = r.headers_mut();
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(
        header::SEC_WEBSOCKET_ACCEPT,
        HeaderValue::from_str(&accept)?,
    );
    Ok(r)
}

async fn passthrough<C, U>(client: WebSocketStream<C>, upstream: WebSocketStream<U>) -> Result<()>
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut c_tx, mut c_rx) = client.split();
    let (mut u_tx, mut u_rx) = upstream.split();
    loop {
        tokio::select! {
            m = u_rx.next() => match m {
                Some(Ok(m @ (Message::Text(_) | Message::Binary(_) | Message::Close(_)))) => {
                    let close = m.is_close();
                    c_tx.send(m).await?;
                    if close { break }
                }
                Some(Ok(_)) => {}
                _ => break,
            },
            m = c_rx.next() => match m {
                Some(Ok(m @ (Message::Text(_) | Message::Binary(_) | Message::Close(_)))) => {
                    let close = m.is_close();
                    u_tx.send(m).await?;
                    if close { break }
                }
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }
    let _ = c_tx.close().await;
    let _ = u_tx.close().await;
    Ok(())
}

/// Turn a payload into a websocket message the way this connection expects it.
fn frame(codec: Codec, deflater: Option<&mut Deflater>, plain: Vec<u8>) -> Result<Message> {
    if let (Some(d), true) = (deflater, codec.compression != Compression::None) {
        return Ok(Message::Binary(Bytes::from(d.push(&plain)?)));
    }
    Ok(match codec.encoding {
        Encoding::Json => {
            Message::text(String::from_utf8(plain).context("gateway JSON is not UTF-8")?)
        }
        Encoding::Etf => Message::Binary(Bytes::from(plain)),
    })
}

pub async fn bridge<C, U>(
    app: Arc<App>,
    client: WebSocketStream<C>,
    upstream: WebSocketStream<U>,
    codec: Codec,
) -> Result<()>
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tracing::info!(
        "gateway connected ({:?}, {:?})",
        codec.encoding,
        codec.compression
    );
    let (mut c_tx, mut c_rx) = client.split();
    let (mut u_tx, mut u_rx) = upstream.split();
    let mut inflater = Inflater::new(codec.compression)?;
    let mut deflater = Deflater::new(codec.compression)?;
    let mut reloads = app.reloads.subscribe();
    let mut last_presence: Option<Val> = None;
    let mut hide_games = app.cfg().plugins.game_activity_toggle.enabled;

    loop {
        tokio::select! {
            m = u_rx.next() => {
                let data = match m {
                    Some(Ok(Message::Binary(b))) => b,
                    Some(Ok(Message::Text(t))) => Bytes::from(t),
                    Some(Ok(Message::Close(f))) => { let _ = c_tx.send(Message::Close(f)).await; break }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                };
                let Some(plain) = inflater.push(&data)? else { continue };
                let out: Vec<Vec<u8>> = match codec.decode(&plain) {
                    Ok(v) => {
                        app.stats.decoded.fetch_add(1, Ordering::Relaxed);
                        let d = app.on_dispatch(v);
                        if d.drop {
                            Vec::new()
                        } else if !d.changed && d.extra.is_empty() && !app.force_decode {
                            vec![plain]
                        } else {
                            std::iter::once(d.payload).chain(d.extra).map(|p| codec.encode(&p)).collect()
                        }
                    }
                    Err(e) => {
                        app.stats.undecodable.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!("undecodable gateway payload passed through: {e:#}");
                        vec![plain]
                    }
                };
                for p in out {
                    c_tx.send(frame(codec, Some(&mut deflater), p)?).await?;
                }
            }
            m = c_rx.next() => {
                let (data, text) = match m {
                    Some(Ok(Message::Text(t))) => (Bytes::from(t), true),
                    Some(Ok(Message::Binary(b))) => (b, false),
                    Some(Ok(Message::Close(f))) => { let _ = u_tx.send(Message::Close(f)).await; break }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                };
                let send_codec = Codec { encoding: codec.encoding, compression: Compression::None };
                let msg = match send_codec.decode(&data) {
                    Ok(mut v) => {
                        if v.get("op").and_then(Val::as_i64) == Some(3) {
                            last_presence = Some(v.clone());
                        }
                        if app.on_send(&mut v) { frame(send_codec, None, send_codec.encode(&v))? } else if text {
                            Message::text(String::from_utf8_lossy(&data).into_owned())
                        } else {
                            Message::Binary(data)
                        }
                    }
                    Err(_) if text => Message::text(String::from_utf8_lossy(&data).into_owned()),
                    Err(_) => Message::Binary(data),
                };
                u_tx.send(msg).await?;
            }
            Ok(()) = reloads.changed() => {
                // Re-send your status when the game activity switch flips, so it applies at once.
                let now = app.cfg().plugins.game_activity_toggle.enabled;
                if now != hide_games {
                    hide_games = now;
                    if let Some(mut p) = last_presence.clone() {
                        app.on_send(&mut p);
                        let send_codec = Codec { encoding: codec.encoding, compression: Compression::None };
                        u_tx.send(frame(send_codec, None, send_codec.encode(&p))?).await?;
                        tracing::info!("status re-sent (game activity {})", if now { "hidden" } else { "shown" });
                    }
                }
            }
        }
    }
    let _ = c_tx.close().await;
    let _ = u_tx.close().await;
    Ok(())
}
