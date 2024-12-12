mod memory_cache;
mod disk_cache;

pub use memory_cache::*;
pub use disk_cache::*;

use bytes::Bytes;
use std::time::{Duration, SystemTime};

pub const MAX_MEMORY_CACHE_SIZE: usize = 100 * 1024 * 1024; // 100MB
pub const CACHE_DIR: &str = "video_cache";

#[derive(Clone)]
pub struct CacheItem {
    pub data: Bytes,
    pub content_type: String,
    pub expires_at: SystemTime,
    pub size: usize,
}
