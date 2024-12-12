use crate::cache::{Cache, CacheItem};
use bytes::Bytes;
use futures_util::StreamExt;
use hyper::{Body, Client, Request, Response, StatusCode};
use hyper::header::{HeaderValue, CONTENT_TYPE};
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use std::time::{Duration, SystemTime};
use tokio::time::timeout;

// 默认超时时间（10秒）
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
// 最大重试次数
const MAX_RETRIES: u32 = 3;
// 重试延迟（基础值，会随重试次数增加）
const RETRY_DELAY: Duration = Duration::from_millis(500);

pub type SharedCache = Arc<RwLock<Cache>>;

#[derive(Debug)]
enum ProxyError {
    Timeout,
    NetworkError(hyper::Error),
    StatusError(StatusCode),
}

impl From<hyper::Error> for ProxyError {
    fn from(err: hyper::Error) -> Self {
        ProxyError::NetworkError(err)
    }
}

async fn handle_video_stream(
    client_response: Response<Body>,
) -> Result<Bytes, ProxyError> {
    let mut body = Vec::new();
    let mut stream = client_response.into_body();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ProxyError::NetworkError)?;
        body.extend_from_slice(&chunk);
    }

    Ok(Bytes::from(body))
}

async fn fetch_with_retry(
    client: &Client<hyper::client::HttpConnector>,
    req: Request<Body>,
    retry_count: u32,
) -> Result<Response<Body>, ProxyError> {
    let mut current_retry = 0;
    let mut last_error = None;

    while current_retry <= retry_count {
        if current_retry > 0 {
            // 指数退避策略：每次重试延迟时间翻倍
            let delay = RETRY_DELAY * (2_u32.pow(current_retry - 1));
            tokio::time::sleep(delay).await;
            println!("重试请求 {}/{}, URI: {}", current_retry, retry_count, req.uri());
        }

        match timeout(REQUEST_TIMEOUT, client.request(req.try_clone().unwrap())).await {
            Ok(result) => {
                match result {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            return Ok(response);
                        } else {
                            last_error = Some(ProxyError::StatusError(status));
                        }
                    }
                    Err(e) => last_error = Some(ProxyError::NetworkError(e)),
                }
            }
            Err(_) => last_error = Some(ProxyError::Timeout),
        }

        current_retry += 1;
    }

    Err(last_error.unwrap_or(ProxyError::NetworkError(hyper::Error::new_canceled())))
}

pub async fn handle_request(
    req: Request<Body>,
    cache: SharedCache,
    client: Client<hyper::client::HttpConnector>,
    semaphore: Arc<Semaphore>,
) -> Result<Response<Body>, hyper::Error> {
    let _permit = semaphore.acquire().await.unwrap();
    let uri = req.uri().to_string();
    
    // 检查缓存
    if let Some(cached_item) = cache.write().await.get(&uri).await {
        println!("缓存命中: {}", uri);
        let mut response = Response::new(Body::from(cached_item.data));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&cached_item.content_type).unwrap(),
        );
        return Ok(response);
    }

    println!("缓存未命中，从源站获取: {}", uri);
    
    // 使用重试机制从源站获取数据
    let source_response = match fetch_with_retry(&client, req, MAX_RETRIES).await {
        Ok(response) => response,
        Err(err) => {
            println!("请求失败 {}: {:?}", uri, err);
            return Ok(Response::builder()
                .status(match err {
                    ProxyError::Timeout => StatusCode::GATEWAY_TIMEOUT,
                    ProxyError::NetworkError(_) => StatusCode::BAD_GATEWAY,
                    ProxyError::StatusError(status) => status,
                })
                .body(Body::empty())
                .unwrap());
        }
    };

    let content_type = source_response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    // 处理视频流
    let video_data = match handle_video_stream(source_response).await {
        Ok(data) => data,
        Err(err) => {
            println!("处理视频流失败 {}: {:?}", uri, err);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::empty())
                .unwrap());
        }
    };
    
    // 存入缓存
    let cache_item = CacheItem {
        data: video_data.clone(),
        content_type: content_type.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(3600),
        size: video_data.len(),
    };
    
    cache.write().await.put(uri, cache_item).await;
    
    // 构建响应
    let mut response = Response::new(Body::from(video_data));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap(),
    );
    
    Ok(response)
}
