use video_proxy_server::proxy::ProxyServer;
use std::net::SocketAddr;

// 默认配置
const DEFAULT_PORT: u16 = 3000;
// 默认缓存容量 
const DEFAULT_CACHE_CAPACITY: usize = 100;
// 默认最大连接数
const DEFAULT_MAX_CONNECTIONS: usize = 50;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT));
    let server = ProxyServer::new(
        addr, 
        DEFAULT_CACHE_CAPACITY, 
        DEFAULT_MAX_CONNECTIONS
    ).await?;
    
    println!("启动视频代理服务器...");
    server.run().await?;
    Ok(())
}
