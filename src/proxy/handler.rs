use crate::cache::{Cache, CacheItem};
use bytes::Bytes;
use http::StatusCode;
use http::header::{CONTENT_TYPE, RANGE, CONTENT_LENGTH, CONTENT_RANGE};
use hyper::body::Incoming;
use hyper::{Request, Response, Method, Uri};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use http_body_util::{BodyExt, Full};
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use std::time::{Duration, SystemTime};
use tokio::time::timeout;
use std::fmt;
use std::error::Error;

// 默认超时时间（10秒）
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
// 最大重试次数
const MAX_RETRIES: u32 = 3;
// 重试延迟（基础值，会随重试次数增加）
const RETRY_DELAY: Duration = Duration::from_millis(500);

pub type SharedCache = Arc<RwLock<Cache>>;

#[derive(Debug)]
pub enum ProxyError {
    Timeout,
    NetworkError(Box<dyn Error + Send + Sync>),
    StatusError(StatusCode),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyError::Timeout => write!(f, "Request timeout"),
            ProxyError::NetworkError(e) => write!(f, "Network error: {}", e),
            ProxyError::StatusError(status) => write!(f, "HTTP error: {}", status),
        }
    }
}

impl Error for ProxyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ProxyError::NetworkError(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<hyper::Error> for ProxyError {
    fn from(err: hyper::Error) -> Self {
        ProxyError::NetworkError(Box::new(err))
    }
}

pub async fn handle_request(
    req: Request<Incoming>,
    cache: SharedCache,
    client: Client<HttpConnector, Full<Bytes>>,
    semaphore: Arc<Semaphore>,
) -> Result<Response<Full<Bytes>>, Box<dyn Error + Send + Sync>> {
    // 获取信号量许可
    let _permit = semaphore.acquire().await?;

    // 处理请求
    match handle_video_stream(req, cache, client).await {
        Ok(response) => Ok(response),
        Err(e) => {
            eprintln!("Error handling video stream: {:?}", e);
            let response = Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::new()))?;
            Ok(response)
        }
    }
}

async fn handle_video_stream(
    req: Request<Incoming>,
    cache: SharedCache,
    client: Client<HttpConnector, Full<Bytes>>,
) -> Result<Response<Full<Bytes>>, Box<dyn Error + Send + Sync>> {
    let uri = req.uri().clone();
    let method = req.method().clone();
    let headers = req.headers().clone();

    // 只处理 GET 请求
    if method != Method::GET {
        return Ok(Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Full::new(Bytes::new()))?);
    }

    // 从缓存中获取数据
    let cache_key = uri.to_string();
    let mut cache_read = cache.write().await;
    if let Some(cache_item) = cache_read.get(&cache_key).await {
        // 处理 Range 请求
        if let Some(range_header) = headers.get(RANGE) {
            if let Ok(range_str) = range_header.to_str() {
                if let Some((start, end)) = parse_range(range_str, cache_item.data.len()) {
                    let data = cache_item.data.slice(start..end + 1);
                    let content_length = end - start + 1;
                    let content_range = format!("bytes {}-{}/{}", start, end, cache_item.data.len());

                    return Ok(Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(CONTENT_TYPE, &cache_item.content_type)
                        .header(CONTENT_LENGTH, content_length.to_string())
                        .header(CONTENT_RANGE, content_range)
                        .body(Full::new(data))?);
                }
            }
        }

        // 返回完整响应
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, &cache_item.content_type)
            .header(CONTENT_LENGTH, cache_item.data.len().to_string())
            .body(Full::new(cache_item.data.clone()))?);
    }
    drop(cache_read);

    // 如果缓存中没有，从源服务器获取
    let (status, headers, body_bytes) = fetch_with_retry(&client, &uri).await?;

    // 如果是成功的响应，则缓存
    if status.is_success() {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let cache_item = CacheItem {
            data: body_bytes.clone(),
            content_type,
            expires_at: SystemTime::now() + Duration::from_secs(3600),
            size: body_bytes.len(),
        };

        let mut cache_write = cache.write().await;
        cache_write.put(cache_key, cache_item).await;
        drop(cache_write);
    }

    // 构建响应
    let mut response_builder = Response::builder().status(status);
    for (key, value) in headers.iter() {
        response_builder = response_builder.header(key, value);
    }

    Ok(response_builder.body(Full::new(body_bytes))?)
}

async fn fetch_with_retry(
    client: &Client<HttpConnector, Full<Bytes>>,
    uri: &Uri,
) -> Result<(StatusCode, http::HeaderMap, Bytes), Box<dyn Error + Send + Sync>> {
    let mut retry_count = 0;
    let mut last_error = None;

    while retry_count < MAX_RETRIES {
        let request = Request::builder()
            .uri(uri)
            .method(Method::GET)
            .body(Full::new(Bytes::new()))?;

        match timeout(REQUEST_TIMEOUT, client.request(request)).await {
            Ok(result) => match result {
                Ok(response) => {
                    let status = response.status();
                    let headers = response.headers().clone();
                    if status.is_success() {
                        let body_bytes = collect_body(response).await?;
                        return Ok((status, headers, body_bytes));
                    }
                    last_error = Some(ProxyError::StatusError(status));
                }
                Err(e) => last_error = Some(ProxyError::NetworkError(Box::new(e))),
            },
            Err(_) => last_error = Some(ProxyError::Timeout),
        }

        retry_count += 1;
        if retry_count < MAX_RETRIES {
            tokio::time::sleep(RETRY_DELAY * retry_count).await;
        }
    }

    Err(Box::new(last_error.unwrap()))
}

async fn collect_body(response: Response<Incoming>) -> Result<Bytes, Box<dyn Error + Send + Sync>> {
    let body = response.into_body();
    let collected = body.collect().await?;
    Ok(collected.to_bytes())
}

fn parse_range(range_str: &str, total_size: usize) -> Option<(usize, usize)> {
    if let Some(range_value) = range_str.strip_prefix("bytes=") {
        let parts: Vec<&str> = range_value.split('-').collect();
        if parts.len() == 2 {
            let start = parts[0].parse::<usize>().ok()?;
            let end = if parts[1].is_empty() {
                total_size - 1
            } else {
                parts[1].parse::<usize>().ok()?
            };

            if start <= end && end < total_size {
                return Some((start, end));
            }
        }
    }
    None
}
