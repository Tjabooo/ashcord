//! `ashcord selftest`: run the proxy on a spare port and talk to the real Discord through it, without
//! logging in. Checks that interception works, that the CA chain is accepted, and that every gateway
//! encoding and compression survives a decode → re-encode round trip.

use crate::app::App;
use crate::codec::{Codec, Inflater};
use crate::config::{Config, Paths};
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::Message;

const COMBOS: &[&str] = &[
    "encoding=json&compress=zlib-stream",
    "encoding=json&compress=zstd-stream",
    "encoding=etf&compress=zlib-stream",
    "encoding=etf&compress=zstd-stream",
    "encoding=json",
    "encoding=etf",
];

/// Open a CONNECT tunnel through the proxy and do TLS trusting ONLY the local CA, so the check fails
/// unless Ashcord really intercepted the connection.
async fn via_proxy(proxy: &str, host: &str, only_ca: Arc<rustls::ClientConfig>) -> Result<TlsStream<TcpStream>> {
    let mut s = TcpStream::connect(proxy).await?;
    s.write_all(format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n").as_bytes()).await?;
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).await? == 0 {
            bail!("proxy closed the CONNECT");
        }
        head.push(b[0]);
    }
    if !head.starts_with(b"HTTP/1.1 200") {
        bail!("proxy answered {}", String::from_utf8_lossy(&head).lines().next().unwrap_or(""));
    }
    Ok(tokio_rustls::TlsConnector::from(only_ca).connect(ServerName::try_from(host.to_string())?, s).await?)
}

pub async fn run(paths: Paths) -> Result<()> {
    let app = App::new(paths, Config::default(), true)?;
    let listener = crate::proxy::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    tokio::spawn(crate::proxy::serve(app.clone(), listener));

    let mut roots = rustls::RootCertStore::empty();
    roots.add(app.ca.ca_der.clone())?;
    let mut cc = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cc.alpn_protocols = vec![b"http/1.1".to_vec()];
    let only_ca = Arc::new(cc);
    let mut failed = 0;

    // REST through the proxy.
    let rest = async {
        let tls = via_proxy(&addr, "discord.com", only_ca.clone()).await?;
        let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await?;
        tokio::spawn(conn);
        let req = hyper::Request::get("/api/v10/gateway")
            .header("host", "discord.com")
            .header("user-agent", "Ashcord selftest")
            .body(crate::rest::empty())?;
        let r = tx.send_request(req).await?;
        let status = r.status();
        let body = r.into_body().collect().await?.to_bytes();
        if !status.is_success() || !body.windows(6).any(|w| w == b"wss://") {
            bail!("HTTP {status}: {}", String::from_utf8_lossy(&body));
        }
        anyhow::Ok(())
    };
    match tokio::time::timeout(Duration::from_secs(15), rest).await {
        Ok(Ok(())) => println!("✓ REST: discord.com intercepted, local CA accepted, API reachable"),
        Ok(Err(e)) => { failed += 1; println!("✗ REST: {e:#}") }
        Err(_) => { failed += 1; println!("✗ REST: timed out") }
    }

    // Gateway: every encoding × compression, decoded by Ashcord, re-encoded, decoded again here.
    for q in COMBOS {
        let one = async {
            let tls = via_proxy(&addr, "gateway.discord.gg", only_ca.clone()).await?;
            let url = format!("wss://gateway.discord.gg/?v=10&{q}");
            let (mut ws, _) = tokio_tungstenite::client_async(url, tls).await.context("handshake")?;
            let codec = Codec::from_query(q)?;
            let mut inflater = Inflater::new(codec.compression)?;
            loop {
                let data = match ws.next().await.context("closed before HELLO")?? {
                    Message::Binary(b) => b.to_vec(),
                    Message::Text(t) => t.as_bytes().to_vec(),
                    _ => continue,
                };
                if let Some(plain) = inflater.push(&data)? {
                    let v = codec.decode(&plain)?;
                    let op = v.get("op").and_then(|o| o.as_i64());
                    let hb = v.at(&["d", "heartbeat_interval"]).and_then(|h| h.as_i64());
                    let _ = ws.close(None).await;
                    return match (op, hb) {
                        (Some(10), Some(ms)) => anyhow::Ok(ms),
                        _ => bail!("expected HELLO, got op {op:?}"),
                    };
                }
            }
        };
        match tokio::time::timeout(Duration::from_secs(15), one).await {
            Ok(Ok(ms)) => println!("✓ gateway {q}: HELLO round-tripped (heartbeat {ms} ms)"),
            Ok(Err(e)) => { failed += 1; println!("✗ gateway {q}: {e:#}") }
            Err(_) => { failed += 1; println!("✗ gateway {q}: timed out") }
        }
    }
    let bad = app.stats.undecodable.load(Ordering::Relaxed);
    if bad > 0 {
        failed += 1;
        println!("✗ {bad} gateway payload(s) Ashcord could not decode");
    }
    println!("  proxy decoded {} payload(s)", app.stats.decoded.load(Ordering::Relaxed));
    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    println!("all checks passed");
    Ok(())
}
