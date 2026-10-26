//! Bridge between the immediate-mode UI thread and async database work.

use crate::db::{self, ssh::Tunnel, Canceller, DriverBox};
use crate::model::{ConnectionConfig, Dialect, DriverKind};
use anyhow::Result;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().thread_name("strata-io").build().expect("tokio runtime")
});

pub fn rt() -> &'static tokio::runtime::Runtime {
    &RUNTIME
}

/// A value being computed in the background. Poll it once per frame.
pub struct Task<T> {
    rx: std::sync::mpsc::Receiver<T>,
    pub started: Instant,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn<F>(ctx: &egui::Context, fut: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        rt().spawn(async move {
            let v = fut.await;
            let _ = tx.send(v);
            ctx.request_repaint();
        });
        Self { rx, started: Instant::now() }
    }

    pub fn poll(&mut self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

/// Polls `slot`; when its task finishes, clears the slot and returns the value.
pub fn take<T: Send + 'static>(slot: &mut Option<Task<T>>) -> Option<T> {
    let v = slot.as_mut()?.poll()?;
    *slot = None;
    Some(v)
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub at: chrono::DateTime<chrono::Local>,
    pub text: String,
    pub ok: bool,
}

pub type SharedDriver = Arc<tokio::sync::Mutex<DriverBox>>;

/// An open connection: the driver behind an async mutex, its tunnel, and the command log.
pub struct Session {
    pub cfg: ConnectionConfig,
    pub server_version: Option<String>,
    pub driver: SharedDriver,
    pub canceller: Option<Canceller>,
    pub log: Arc<Mutex<Vec<LogLine>>>,
    /// Where the database is reachable from this machine (the tunnel's local end when using SSH).
    pub endpoint: (String, u16),
    _tunnel: Option<Tunnel>,
}

impl Session {
    pub async fn open(cfg: ConnectionConfig) -> Result<Session> {
        let (mut driver, tunnel) = db::connect(&cfg).await?;
        let server_version = match tokio::time::timeout(std::time::Duration::from_secs(5), driver.server_version()).await {
            Ok(Ok(version)) => Some(version),
            _ => None,
        };
        let canceller = driver.canceller();
        let endpoint = match &tunnel {
            Some(t) => ("127.0.0.1".to_string(), t.local_port),
            None => (cfg.host.clone(), cfg.port),
        };
        Ok(Session { cfg, server_version, driver: Arc::new(tokio::sync::Mutex::new(driver)), canceller, log: Arc::new(Mutex::new(Vec::new())), endpoint, _tunnel: tunnel })
    }

    pub fn kind(&self) -> DriverKind {
        self.cfg.driver
    }

    pub fn dialect(&self) -> Dialect {
        self.cfg.driver.dialect()
    }

    pub fn log(&self, text: impl Into<String>, ok: bool) {
        if let Ok(mut l) = self.log.lock() {
            l.push(LogLine { at: chrono::Local::now(), text: text.into(), ok });
            let len = l.len();
            if len > 5000 {
                l.drain(0..len - 5000);
            }
        }
    }

    /// Runs `f` with exclusive access to the driver on the IO runtime.
    pub fn spawn<T, F, Fut>(&self, ctx: &egui::Context, f: F) -> Task<Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(SharedDriver) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let d = self.driver.clone();
        Task::spawn(ctx, async move { f(d).await })
    }

    pub fn cancel(&self) {
        if let Some(c) = &self.canceller {
            c();
        }
    }
}

/// Checks whether a connection works without keeping it.
pub async fn test_connection(cfg: ConnectionConfig) -> Result<String> {
    let started = Instant::now();
    let (mut d, _t) = db::connect(&cfg).await?;
    d.ping().await?;
    let dbs = d.databases().await.map(|v| v.len()).unwrap_or(0);
    d.close().await;
    Ok(format!("Connected in {} ms · {} database{}", started.elapsed().as_millis(), dbs, if dbs == 1 { "" } else { "s" }))
}
