pub mod mongodb;
pub mod postgres;
pub mod s3;

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;
use tokio::sync::RwLock;

use crate::config::PresetConfig;
use crate::error::Result;

#[derive(Clone)]
pub enum CachedClient {
    Postgres(PgPool),
    Mongodb(::mongodb::Client),
    S3(aws_sdk_s3::Client),
}

struct CachedEntry {
    config: PresetConfig,
    client: CachedClient,
}

#[derive(Default)]
pub struct ConnectorCache {
    entries: RwLock<HashMap<String, CachedEntry>>,
}

impl ConnectorCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_init(&self, name: &str, cfg: &PresetConfig) -> Result<CachedClient> {
        {
            let guard = self.entries.read().await;
            if let Some(entry) = guard.get(name)
                && &entry.config == cfg
            {
                return Ok(entry.client.clone());
            }
        }

        let new_client = build_client(cfg).await?;

        let replaced = {
            let mut guard = self.entries.write().await;
            if let Some(entry) = guard.get(name)
                && &entry.config == cfg
            {
                return Ok(entry.client.clone());
            }
            guard.insert(
                name.to_string(),
                CachedEntry {
                    config: cfg.clone(),
                    client: new_client.clone(),
                },
            )
        };

        if let Some(old) = replaced {
            close_client(old.client).await;
        }

        Ok(new_client)
    }

    pub async fn evict(&self, name: &str) {
        let removed = self.entries.write().await.remove(name);
        if let Some(entry) = removed {
            close_client(entry.client).await;
        }
    }

    pub async fn retain_active(&self, active_names: &[&str]) {
        let active: HashSet<&str> = active_names.iter().copied().collect();
        let evicted: Vec<CachedEntry> = {
            let mut guard = self.entries.write().await;
            let stale_keys: Vec<String> = guard
                .keys()
                .filter(|k| !active.contains(k.as_str()))
                .cloned()
                .collect();
            stale_keys
                .into_iter()
                .filter_map(|k| guard.remove(&k))
                .collect()
        };

        for entry in evicted {
            close_client(entry.client).await;
        }
    }
}

async fn build_client(cfg: &PresetConfig) -> Result<CachedClient> {
    match cfg {
        PresetConfig::Postgres { .. } => postgres::build_pool(cfg).map(CachedClient::Postgres),
        PresetConfig::Mongodb { .. } => mongodb::build_client(cfg).await.map(CachedClient::Mongodb),
        PresetConfig::S3 { .. } => s3::build_client(cfg).map(CachedClient::S3),
    }
}

async fn close_client(client: CachedClient) {
    if let CachedClient::Postgres(pool) = client {
        pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg_cfg(host: &str) -> PresetConfig {
        PresetConfig::Postgres {
            host: host.to_string(),
            port: 5432,
            user: "admin".to_string(),
            password: "secret".to_string(),
            database: "app".to_string(),
        }
    }

    #[tokio::test]
    async fn caches_reuses_invalidates_and_retains() {
        let cache = ConnectorCache::new();
        let cfg1 = pg_cfg("127.0.0.1");

        let c1 = cache.get_or_init("dev-pg", &cfg1).await.unwrap();
        let c2 = cache.get_or_init("dev-pg", &cfg1).await.unwrap();
        match (&c1, &c2) {
            (CachedClient::Postgres(p1), CachedClient::Postgres(p2)) => {
                assert!(!p1.is_closed());
                assert!(!p2.is_closed());
            }
            _ => panic!("expected postgres pools"),
        }

        let cfg2 = pg_cfg("10.0.0.2");
        let c3 = cache.get_or_init("dev-pg", &cfg2).await.unwrap();
        if let CachedClient::Postgres(p1) = c1 {
            assert!(p1.is_closed());
        }
        if let CachedClient::Postgres(p3) = &c3 {
            assert!(!p3.is_closed());
        }

        cache.retain_active(&[]).await;
        if let CachedClient::Postgres(p3) = c3 {
            assert!(p3.is_closed());
        }
    }
}
