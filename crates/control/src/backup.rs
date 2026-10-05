//! Continuous off-site backup of control's database (#174), with
//! [Litestream](https://litestream.io) to any S3-compatible store
//! (the hosted control uses Cloudflare R2).
//!
//! Off unless `LITESTREAM_BUCKET` is set. Then, before control opens its
//! database:
//!
//! - **No database yet** (a new volume, a lost one): restore the latest
//!   backup into place, if there is one. A store that can't be reached
//!   stops control from starting, rather than starting empty and backing
//!   that up over the real thing.
//! - **Then** run `litestream replicate` beside control, restarting it if
//!   it stops, and stop it cleanly (its last sync) when control stops.
//!
//! The keys go to Litestream in its own environment variables
//! (`LITESTREAM_ACCESS_KEY_ID`, `LITESTREAM_SECRET_ACCESS_KEY`), never in
//! the config file control writes next to the database.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context as _;
use tracing::{error, info, warn};

#[derive(clap::Args, Debug, Clone)]
pub struct Litestream {
    /// The bucket to back the database up to; unset, no backup.
    #[arg(long = "litestream-bucket", env = "LITESTREAM_BUCKET")]
    pub bucket: Option<String>,
    /// The store's S3 endpoint (R2: `https://<account id>.r2.cloudflarestorage.com`);
    /// unset for AWS S3.
    #[arg(long = "litestream-endpoint", env = "LITESTREAM_ENDPOINT")]
    pub endpoint: Option<String>,
    /// Where in the bucket.
    #[arg(long = "litestream-path", env = "LITESTREAM_PATH", default_value = "control")]
    pub path: String,
    #[arg(long = "litestream-region", env = "LITESTREAM_REGION", default_value = "auto")]
    pub region: String,
    /// The litestream binary.
    #[arg(long = "litestream-bin", env = "ILLOGICAL_LITESTREAM", default_value = "/litestream")]
    pub bin: PathBuf,
}

pub struct Backup {
    bin: PathBuf,
    config: PathBuf,
    db: PathBuf,
    child: Arc<Mutex<Option<u32>>>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
}

impl Backup {
    /// From the arguments: `None` when no bucket is set.
    pub fn new(a: &Litestream, db: &Path) -> anyhow::Result<Option<Self>> {
        let Some(bucket) = a.bucket.clone().filter(|b| !b.is_empty()) else {
            info!("no LITESTREAM_BUCKET: the database isn't backed up off-site");
            return Ok(None);
        };
        for k in ["LITESTREAM_ACCESS_KEY_ID", "LITESTREAM_SECRET_ACCESS_KEY"] {
            if std::env::var(k).map_or(true, |v| v.is_empty()) {
                anyhow::bail!("LITESTREAM_BUCKET is set but {k} isn't");
            }
        }
        let db = std::path::absolute(db)?;
        let config = db.with_file_name("litestream.yml");
        std::fs::write(&config, config_yaml(&db, &bucket, a))
            .with_context(|| format!("writing {}", config.display()))?;
        let bin = if a.bin.exists() { a.bin.clone() } else { PathBuf::from("litestream") };
        info!(bucket, path = a.path, endpoint = a.endpoint.as_deref().unwrap_or("AWS"), "backing up with Litestream");
        Ok(Some(Self { bin, config, db, child: Default::default(), stopping: Default::default() }))
    }

    /// No database: restore the latest backup, if there is one.
    pub async fn restore_if_missing(&self) -> anyhow::Result<()> {
        if self.db.exists() {
            return Ok(());
        }
        info!(db = %self.db.display(), "no database: restoring the latest backup, if any");
        let out = tokio::process::Command::new(&self.bin)
            .arg("restore")
            .arg("-config")
            .arg(&self.config)
            .args(["-if-db-not-exists", "-if-replica-exists"])
            .arg(&self.db)
            .output()
            .await
            .with_context(|| format!("running {}", self.bin.display()))?;
        let said = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        if !out.status.success() {
            anyhow::bail!("restoring the database from its backup failed ({}): {said}", out.status);
        }
        if self.db.exists() {
            info!("database restored from its backup");
        } else {
            info!("no backup yet: starting a new database");
        }
        Ok(())
    }

    /// Replicate for as long as control runs.
    pub fn replicate(&self) {
        let (bin, config, child, stopping) =
            (self.bin.clone(), self.config.clone(), self.child.clone(), self.stopping.clone());
        tokio::spawn(async move {
            loop {
                let spawned = tokio::process::Command::new(&bin)
                    .arg("replicate")
                    .arg("-config")
                    .arg(&config)
                    // Its own process group: a signal to control's group
                    // doesn't cut its last sync short; control stops it.
                    .process_group(0)
                    .kill_on_drop(true)
                    .spawn();
                match spawned {
                    Ok(mut c) => {
                        *child.lock().unwrap() = c.id();
                        let status = c.wait().await;
                        *child.lock().unwrap() = None;
                        if stopping.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        error!(?status, "litestream stopped: restarting it");
                    }
                    Err(e) => error!(error = %e, bin = %bin.display(), "can't start litestream"),
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    /// Control is stopping: let Litestream sync one last time and stop.
    pub async fn stop(&self) {
        self.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        let Some(pid) = *self.child.lock().unwrap() else { return };
        // SAFETY: a plain signal to our own child.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if self.child.lock().unwrap().is_none() {
                info!("litestream stopped");
                return;
            }
        }
        warn!("litestream didn't stop within 10 s");
    }
}

/// How long the backup keeps anything (#174): the privacy policy promises
/// at most 30 days.
pub const RETENTION_DAYS: u64 = 7;

/// Litestream's config: one database, one S3-compatible replica.
fn config_yaml(db: &Path, bucket: &str, a: &Litestream) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap();
    let mut y = format!(
        "# Written by illogical-control at start (#174); keys come from the environment.\n\
         # A snapshot a day, each kept a week: what's deleted here is gone from\n\
         # the backup within {RETENTION_DAYS} days (the privacy policy says at most 30).\n\
         snapshot:\n  interval: 24h\n  retention: {}h\n\
         dbs:\n  - path: {}\n    replica:\n      type: s3\n      bucket: {}\n      path: {}\n      region: {}\n",
        RETENTION_DAYS * 24,
        q(&db.to_string_lossy()),
        q(bucket),
        q(&a.path),
        q(&a.region),
    );
    if let Some(e) = a.endpoint.as_deref().filter(|e| !e.is_empty()) {
        y += &format!("      endpoint: {}\n      force-path-style: true\n", q(e));
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_names_no_keys() {
        let a = Litestream {
            bucket: Some("b".into()),
            endpoint: Some("https://x.r2.cloudflarestorage.com".into()),
            path: "control".into(),
            region: "auto".into(),
            bin: "/litestream".into(),
        };
        let y = config_yaml(Path::new("/data/control.db"), "b", &a);
        assert!(y.contains("path: \"/data/control.db\""));
        assert!(y.contains("snapshot:\n  interval: 24h\n  retention: 168h\n"), "{y}");
        const { assert!(RETENTION_DAYS <= 30) };
        assert!(y.contains("endpoint: \"https://x.r2.cloudflarestorage.com\""));
        assert!(!y.to_lowercase().contains("secret"));
        let plain = Litestream { endpoint: None, ..a };
        assert!(!config_yaml(Path::new("/d.db"), "b", &plain).contains("endpoint"));
    }
}
