pub mod strategies;

use std::{sync::Arc, time::Duration};

use quick_cache::sync::Cache;
use reqwest::{Client, Proxy};
use thiserror::Error;

use crate::pool::strategies::{RoundRobin, Strategy};

#[derive(Error, Debug)]
pub enum ProxyPoolError {
    #[error("Proxy list is empty!")]
    EmptyProxyList,
    #[error("Reqwest error: {0}")]
    ReqwestError(#[from] reqwest::Error),
}

pub struct ProxyPool {
    proxies: Arc<Vec<Arc<str>>>,
    pool: Cache<Arc<str>, Arc<Client>>,
    strategy: Box<dyn Strategy>,
}

impl ProxyPool {
    pub fn new(capacity: usize, proxies: Vec<Arc<str>>) -> Self {
        Self {
            proxies: Arc::new(proxies),
            pool: Cache::new(capacity),
            strategy: Box::new(RoundRobin::default()),
        }
    }

    pub fn get_proxy(&self) -> Result<&Arc<str>, ProxyPoolError> {
        self.strategy
            .choose(&self.proxies)
            .ok_or(ProxyPoolError::EmptyProxyList)
    }

    pub fn get_reverse_proxy(&self) -> Result<Arc<Client>, ProxyPoolError> {
        let proxy_url = self.get_proxy()?;
        self.pool
            .get_or_insert_with(proxy_url, || {
                let proxy = Proxy::all(proxy_url.as_ref())?;
                Client::builder()
                    .proxy(proxy)
                    .pool_idle_timeout(Duration::from_secs(30))
                    .timeout(Duration::from_secs(10))
                    .build()
                    .map(Arc::new)
            })
            .map_err(Into::into)
    }
}
