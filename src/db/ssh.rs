//! SSH local port forwarding: listen on 127.0.0.1:<random> and pipe each socket through a
//! `direct-tcpip` channel to the database host as seen from the bastion.

use crate::model::SshConfig;
use anyhow::{bail, Context, Result};
use russh::client;
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

struct AcceptingHandler;

impl client::Handler for AcceptingHandler {
    type Error = russh::Error;

    // Host key pinning is not implemented yet; accept like `StrictHostKeyChecking=no`.
    async fn check_server_key(&mut self, _key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct Tunnel {
    pub local_port: u16,
    task: JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Tunnel {
    pub async fn open(ssh: &SshConfig, remote_host: &str, remote_port: u16) -> Result<Self> {
        if ssh.host.trim().is_empty() {
            bail!("SSH host is empty");
        }
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            keepalive_interval: Some(std::time::Duration::from_secs(30)),
            ..Default::default()
        });
        let port = if ssh.port == 0 { 22 } else { ssh.port };
        let mut session = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client::connect(config, (ssh.host.as_str(), port), AcceptingHandler),
        )
        .await
        .context("Can't connect to the SSH server: timed out")?
        .context("Can't connect to the SSH server")?;

        let auth = if ssh.use_key {
            let path = expand_home(&ssh.key_path);
            let pass = (!ssh.key_passphrase.is_empty()).then_some(ssh.key_passphrase.as_str());
            let key = load_secret_key(&path, pass).with_context(|| format!("Can't read private key {path}"))?;
            let hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
            session.authenticate_publickey(&ssh.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?
        } else {
            session.authenticate_password(&ssh.user, &ssh.password).await?
        };
        if !auth.success() {
            bail!("SSH authentication failed");
        }

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_port = listener.local_addr()?.port();
        let session = Arc::new(session);
        let host = remote_host.to_string();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, peer)) = listener.accept().await else { break };
                let session = session.clone();
                let host = host.clone();
                tokio::spawn(async move {
                    match session.channel_open_direct_tcpip(host, remote_port as u32, peer.ip().to_string(), peer.port() as u32).await {
                        Ok(channel) => {
                            let mut stream = channel.into_stream();
                            let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                        }
                        Err(e) => eprintln!("strata: ssh channel failed: {e}"),
                    }
                });
            }
        });
        Ok(Self { local_port, task })
    }
}

pub fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    p.to_string()
}
