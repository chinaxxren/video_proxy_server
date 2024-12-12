use std::net::SocketAddr;
use video_proxy_server::proxy::server::ProxyServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let cache_capacity = 100; // 缓存容量
    let max_connections = 100; // 最大并发连接数

    let server = ProxyServer::new(addr, cache_capacity, max_connections).await?;
    server.run().await?;

    Ok(())
}
