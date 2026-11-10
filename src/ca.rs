//! The local certificate authority. Every install makes its own, and it may only sign certificates for
//! Discord's domains (an X.509 name constraint), so trusting it can't expose any other site.

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, GeneralSubtree, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, NameConstraints,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use time::{Duration, OffsetDateTime};

/// The only names the CA can ever sign for (subdomains included).
pub const CONSTRAINED_TO: &[&str] = &["discord.com", "discordapp.com", "discord.gg"];
pub const CA_NAME: &str = "Ashcord local CA (this computer only)";

pub struct Authority {
    issuer: Issuer<'static, KeyPair>,
    pub ca_der: CertificateDer<'static>,
    pub ca_pem_path: PathBuf,
    leaves: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Authority")
    }
}

fn ca_params() -> Result<CertificateParams> {
    let mut p = CertificateParams::new(Vec::<String>::new())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, CA_NAME);
    dn.push(DnType::OrganizationName, "Ashcord");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: CONSTRAINED_TO.iter().map(|d| GeneralSubtree::DnsName((*d).into())).collect(),
        excluded_subtrees: vec![],
    });
    let now = OffsetDateTime::now_utc();
    p.not_before = now - Duration::days(1);
    p.not_after = now + Duration::days(3650);
    Ok(p)
}

impl Authority {
    /// Load `ca.pem` + `ca.key` from `dir`, creating them on first run.
    pub fn load_or_create(dir: &Path) -> Result<Authority> {
        std::fs::create_dir_all(dir)?;
        let cert_path = dir.join("ca.pem");
        let key_path = dir.join("ca.key");
        if !cert_path.exists() || !key_path.exists() {
            let key = KeyPair::generate()?;
            let cert = ca_params()?.self_signed(&key)?;
            write_private(&key_path, key.serialize_pem().as_bytes())?;
            std::fs::write(&cert_path, cert.pem())?;
            tracing::info!("created a new local CA at {}", cert_path.display());
        }
        let pem = std::fs::read_to_string(&cert_path)?;
        let key = KeyPair::from_pem(&std::fs::read_to_string(&key_path)?).context("reading ca.key")?;
        let issuer = Issuer::from_ca_cert_pem(&pem, key).context("reading ca.pem")?;
        let ca_der = pem_to_der(&pem)?;
        Ok(Authority { issuer, ca_der, ca_pem_path: cert_path, leaves: Mutex::new(HashMap::new()) })
    }

    /// A certificate for `host`, made on first use and kept in memory.
    pub fn leaf(&self, host: &str) -> Result<Arc<CertifiedKey>> {
        let host = host.to_ascii_lowercase();
        if let Some(k) = self.leaves.lock().unwrap().get(&host) {
            return Ok(k.clone());
        }
        let mut p = CertificateParams::new(vec![host.clone()])?;
        p.distinguished_name.push(DnType::CommonName, host.clone());
        p.is_ca = IsCa::NoCa;
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.use_authority_key_identifier_extension = true;
        let now = OffsetDateTime::now_utc();
        p.not_before = now - Duration::days(1);
        p.not_after = now + Duration::days(90);
        let key = KeyPair::generate()?;
        let cert = p.signed_by(&key, &self.issuer)?;
        let signer = rustls::crypto::ring::sign::any_supported_type(&PrivateKeyDer::Pkcs8(
            PrivatePkcs8KeyDer::from(key.serialize_der()),
        ))?;
        let ck = Arc::new(CertifiedKey::new(vec![cert.der().clone(), self.ca_der.clone()], signer));
        self.leaves.lock().unwrap().insert(host, ck.clone());
        Ok(ck)
    }

    pub fn server_config(self: &Arc<Self>) -> Arc<rustls::ServerConfig> {
        let mut c = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Resolver(self.clone())));
        c.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(c)
    }
}

#[derive(Debug)]
struct Resolver(Arc<Authority>);

impl ResolvesServerCert for Resolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = hello.server_name()?;
        match self.0.leaf(name) {
            Ok(k) => Some(k),
            Err(e) => {
                tracing::warn!("no certificate for {name}: {e:#}");
                None
            }
        }
    }
}

fn pem_to_der(pem: &str) -> Result<CertificateDer<'static>> {
    use rustls::pki_types::pem::PemObject;
    CertificateDer::from_pem_slice(pem.as_bytes()).context("ca.pem is not a PEM certificate")
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(data)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::client::WebPkiServerVerifier;
    use rustls::client::danger::ServerCertVerifier;
    use rustls::pki_types::{ServerName, UnixTime};

    fn verifier(a: &Authority) -> Arc<WebPkiServerVerifier> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(a.ca_der.clone()).unwrap();
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider()))
            .build()
            .unwrap()
    }

    #[test]
    fn signs_discord_and_survives_reload() {
        let dir = std::env::temp_dir().join(format!("ashcord-ca-{}", std::process::id()));
        let a = Authority::load_or_create(&dir).unwrap();
        // Reloading from disk must give the same CA, and its leaves must chain to the saved ca.pem.
        let b = Authority::load_or_create(&dir).unwrap();
        assert_eq!(a.ca_der, b.ca_der);
        let v = verifier(&a);
        for host in ["discord.com", "gateway.discord.gg", "canary.discord.com"] {
            let leaf = b.leaf(host).unwrap();
            v.verify_server_cert(&leaf.cert[0], &[], &ServerName::try_from(host).unwrap(), &[], UnixTime::now())
                .unwrap_or_else(|e| panic!("{host}: {e}"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn name_constraint_blocks_other_sites() {
        let dir = std::env::temp_dir().join(format!("ashcord-ca-nc-{}", std::process::id()));
        let a = Authority::load_or_create(&dir).unwrap();
        let leaf = a.leaf("example.com").unwrap();
        let r = verifier(&a).verify_server_cert(
            &leaf.cert[0],
            &[],
            &ServerName::try_from("example.com").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(r.is_err(), "a certificate for example.com must not validate");
        std::fs::remove_dir_all(&dir).ok();
    }
}
