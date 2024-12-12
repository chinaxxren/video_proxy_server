use crate::cache::{Cache, CacheItem};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use hyper::{Body, Client, Request, Response, StatusCode};
use hyper::header::{HeaderValue, CONTENT_TYPE, RANGE, CONTENT_LENGTH, CONTENT_RANGE, HeaderMap};
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
// 预加载大小（1MB）
const PRELOAD_SIZE: u64 = 1024 * 1024;

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

// 新增：表示部分加载的视频数据
#[derive(Clone)]
struct PartialVideoData {
    data: Bytes,
    content_type: String,
    total_size: u64,
    loaded_range: (u64, u64),
}

async fn handle_video_stream(
    client_response: Response<Body>,
    range: Option<(u64, u64)>,
) -> Result<(Bytes, u64), ProxyError> {
    let total_size = client_response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let mut body = BytesMut::new();
    let mut stream = client_response.into_body();
    let mut bytes_received = 0u64;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ProxyError::NetworkError)?;
        bytes_received += chunk.len() as u64;
        
        // 如果指定了范围且已达到结束位置，停止加载
        if let Some((_, end)) = range {
            if bytes_received > end {
                break;
            }
        }
        
        body.extend_from_slice(&chunk);
    }

    Ok((body.freeze(), total_size))
}

fn create_request(
    method: &hyper::Method,
    uri: &hyper::Uri,
    headers: &HeaderMap,
    range: Option<(u64, u64)>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri);

    // 复制原始请求的相关头部
    for (key, value) in headers.iter() {
        if key != RANGE {  // 不复制 Range 头，因为我们要自己设置
            builder = builder.header(key, value);
        }
    }

    // 如果有范围要求，添加 Range 头部
    if let Some((start, end)) = range {
        builder = builder.header(RANGE, format!("bytes={}-{}", start, end));
    }

    builder.body(Body::empty()).unwrap()
}

async fn fetch_with_retry(
    client: &Client<hyper::client::HttpConnector>,
    original_req: &Request<Body>,
    range: Option<(u64, u64)>,
    retry_count: u32,
) -> Result<Response<Body>, ProxyError> {
    let mut current_retry = 0;
    let mut last_error = None;

    while current_retry <= retry_count {
        if current_retry > 0 {
            let delay = RETRY_DELAY * (2_u32.pow(current_retry - 1));
            tokio::time::sleep(delay).await;
            println!("重试请求 {}/{}, URI: {}", current_retry, retry_count, original_req.uri());
        }

        let req = create_request(
            original_req.method(),
            original_req.uri(),
            original_req.headers(),
            range,
        );

        match timeout(REQUEST_TIMEOUT, client.request(req)).await {
            Ok(result) => {
                match result {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() || status == StatusCode::PARTIAL_CONTENT {
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

    Err(last_error.unwrap_or(ProxyError::Timeout))
}

pub async fn handle_request(
    req: Request<Body>,
    cache: SharedCache,
    client: Client<hyper::client::HttpConnector>,
    semaphore: Arc<Semaphore>,
) -> Result<Response<Body>, hyper::Error> {
    let _permit = semaphore.acquire().await.unwrap();
    let uri = req.uri().to_string();
    
    // 解析客户端请求的范围
    let client_range = req.headers()
        .get(RANGE)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| {
            let parts: Vec<&str> = s.trim_start_matches("bytes=").split('-').collect();
            if parts.len() == 2 {
                let start = parts[0].parse::<u64>().ok()?;
                let end = parts[1].parse::<u64>().ok()?;
                Some((start, end))
            } else {
                None
            }
        });

    // 检查缓存
    if let Some(cached_item) = cache.write().await.get(&uri).await {
        println!("缓存命中: {}", uri);
        
        // 如果客户端请求特定范围，返回相应部分
        if let Some((start, end)) = client_range {
            let mut response = Response::new(Body::from(
                cached_item.data.slice(start as usize..std::cmp::min(end as usize + 1, cached_item.data.len()))
            ));
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_str(&cached_item.content_type).unwrap(),
            );
            response.headers_mut().insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {}-{}/{}", 
                    start, 
                    std::cmp::min(end, cached_item.data.len() as u64 - 1),
                    cached_item.data.len()
                )).unwrap(),
            );
            response.headers_mut().insert(
                CONTENT_LENGTH,
                HeaderValue::from_str(&format!("{}", 
                    std::cmp::min(end - start + 1, cached_item.data.len() as u64 - start)
                )).unwrap(),
            );
            return Ok(response);
        }
        
        let mut response = Response::new(Body::from(cached_item.data));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&cached_item.content_type).unwrap(),
        );
        return Ok(response);
    }

    println!("缓存未命中，从源站获取: {}", uri);
    
    // 首次请求只获取预加载部分
    let initial_range = Some((0, PRELOAD_SIZE - 1));
    let source_response = match fetch_with_retry(&client, &req, initial_range, MAX_RETRIES).await {
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
    let (video_data, total_size) = match handle_video_stream(source_response, initial_range).await {
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
    response.headers_mut().insert(
        CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes 0-{}/{}", 
            PRELOAD_SIZE - 1,
            total_size
        )).unwrap(),
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&PRELOAD_SIZE.to_string()).unwrap(),
    );
    
    Ok(response)
}
