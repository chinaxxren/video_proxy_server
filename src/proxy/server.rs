use crate::cache::Cache;
use crate::proxy::handler::{handle_request, SharedCache};
use hyper::{Client, Server};
use hyper::service::{make_service_fn, service_fn};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, Semaphore};
use std::net::SocketAddr;
use std::io;

pub struct ProxyServer {
    addr: SocketAddr,
    cache: SharedCache,
    client: Client<hyper::client::HttpConnector>,
    semaphore: Arc<Semaphore>,
}

impl ProxyServer {
    pub async fn new(addr: SocketAddr, cache_capacity: usize, max_connections: usize) -> io::Result<Self> {
        let cache = Arc::new(RwLock::new(Cache::new(cache_capacity, Duration::from_secs(3600)).await?));
        let client = Client::new();
        let semaphore = Arc::new(Semaphore::new(max_connections));

        Ok(Self {
            addr,
            cache,
            client,
            semaphore,
        })
    }

    pub async fn run(&self) -> Result<(), hyper::Error> {
        let make_svc = make_service_fn(move |_conn| {
            let cache = self.cache.clone();
            let client = self.client.clone();
            let semaphore = self.semaphore.clone();
            
            async move {
                Ok::<_, hyper::Error>(service_fn(move |req| {
                    handle_request(
                        req,
                        cache.clone(),
                        client.clone(),
                        semaphore.clone(),
                    )
                }))
            }
        });

        let server = Server::bind(&self.addr).serve(make_svc);

        println!("视频代理服务器运行在 http://{}", self.addr);
        println!("- 缓存过期时间: 1小时");
        println!("- 最大并发连接: {}", self.semaphore.available_permits());

        server.await?;

        Ok(())
    }
}
