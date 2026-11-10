//! Add the local CA to the certificate stores Discord reads (and remove it again).

use crate::ca::CA_NAME;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(cmd: &mut Command) -> Result<()> {
    let what = format!("{cmd:?}");
    let out = cmd.output().with_context(|| format!("running {what}"))?;
    if !out.status.success() {
        bail!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// NSS databases Chromium/Electron builds of Discord read on Linux (native, Flatpak, Snap).
#[cfg(all(unix, not(target_os = "macos")))]
fn nss_dbs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut v = vec![home.join(".pki/nssdb")];
    for app in ["com.discordapp.Discord", "com.discordapp.DiscordCanary", "dev.vencord.Vesktop"] {
        let base = home.join(".var/app").join(app);
        if base.exists() {
            v.push(base.join(".pki/nssdb"));
        }
    }
    let snap = home.join("snap/discord/current");
    if snap.exists() {
        v.push(snap.join(".pki/nssdb"));
    }
    v
}

pub fn install(ca: &Path) -> Result<()> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if Command::new("certutil").arg("-H").output().is_err() {
            bail!("`certutil` is missing: install NSS tools (Arch: nss, Debian/Ubuntu: libnss3-tools, Fedora: nss-tools)");
        }
        for db in nss_dbs() {
            if !db.join("cert9.db").exists() {
                std::fs::create_dir_all(&db)?;
                run(Command::new("certutil").arg("-d").arg(format!("sql:{}", db.display())).args(["-N", "--empty-password"]))?;
            }
            run(Command::new("certutil")
                .arg("-d")
                .arg(format!("sql:{}", db.display()))
                .args(["-A", "-t", "C,,", "-n", CA_NAME, "-i"])
                .arg(ca))?;
            println!("trusted in {}", db.display());
        }
        println!(
            "\nApps that use the system store instead (possibly Discord's native client) need it there too;\n\
             that takes root, so run it yourself:\n  Arch/Fedora:   sudo trust anchor --store {0}\n  \
             Debian/Ubuntu: sudo cp {0} /usr/local/share/ca-certificates/ashcord.crt && sudo update-ca-certificates",
            ca.display()
        );
    }
    #[cfg(windows)]
    {
        // Current-user store: no admin needed; Windows asks you to confirm once.
        run(Command::new("certutil").args(["-user", "-addstore", "Root"]).arg(ca))?;
        println!("trusted in your Windows certificate store (Current User > Trusted Root)");
    }
    #[cfg(target_os = "macos")]
    {
        let kc = dirs::home_dir().unwrap_or_default().join("Library/Keychains/login.keychain-db");
        run(Command::new("security").args(["add-trusted-cert", "-r", "trustRoot", "-k"]).arg(kc).arg(ca))?;
        println!("trusted in your login keychain");
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    #[cfg(all(unix, not(target_os = "macos")))]
    for db in nss_dbs() {
        if db.join("cert9.db").exists() {
            let _ = run(Command::new("certutil").arg("-d").arg(format!("sql:{}", db.display())).args(["-D", "-n", CA_NAME]));
            println!("removed from {}", db.display());
        }
    }
    #[cfg(windows)]
    run(Command::new("certutil").args(["-user", "-delstore", "Root", CA_NAME]))?;
    #[cfg(target_os = "macos")]
    run(Command::new("security").args(["delete-certificate", "-c", CA_NAME]))?;
    Ok(())
}
