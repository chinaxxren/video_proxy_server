use crate::cache::Cache;
use crate::proxy::handler::{handle_request, SharedCache};
use hyper::{Request, Response, StatusCode, body::Incoming};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto::Builder as ServerBuilder;
use http_body_util::Full;
use bytes::Bytes;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, Semaphore};

pub struct ProxyServer {
    addr: SocketAddr,
    cache: SharedCache,
    client: Client<HttpConnector, Full<Bytes>>,
    semaphore: Arc<Semaphore>,
}

impl ProxyServer {
    pub async fn new(addr: SocketAddr, cache_capacity: usize, max_connections: usize) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let cache = Arc::new(RwLock::new(Cache::new(cache_capacity, Duration::from_secs(3600)).await?));
        let client = Client::builder(hyper_util::rt::TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build_http();
        let semaphore = Arc::new(Semaphore::new(max_connections));

        Ok(Self {
            addr,
            cache,
            client,
            semaphore,
        })
    }

    pub async fn run(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let listener = TcpListener::bind(self.addr).await?;
        println!("视频代理服务器运行在 http://{}", self.addr);
        println!("- 缓存过期时间: 1小时");
        println!("- 最大并发连接: {}", self.semaphore.available_permits());

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);

            let cache = self.cache.clone();
            let client = self.client.clone();
            let semaphore = self.semaphore.clone();

            tokio::task::spawn(async move {
                let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                    let cache = cache.clone();
                    let client = client.clone();
                    let semaphore = semaphore.clone();
                    async move {
                        match handle_request(req, cache, client, semaphore).await {
                            Ok(response) => Ok::<_, hyper::Error>(response),
                            Err(e) => {
                                eprintln!("Error handling request: {:?}", e);
                                Ok(Response::builder()
                                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                                    .body(Full::new(Bytes::new()))
                                    .unwrap())
                            }
                        }
                    }
                });

                if let Err(err) = ServerBuilder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(io, service)
                    .await
                {
                    eprintln!("Error serving connection: {:?}", err);
                }
            });
        }
    }
}
