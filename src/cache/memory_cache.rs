use super::{CacheItem, DiskCache, MAX_MEMORY_CACHE_SIZE};
use bytes::Bytes;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::time::{Duration, SystemTime};
use std::io;

pub struct Cache {
    store: LruCache<String, CacheItem>,
    max_age: Duration,
    disk_cache: DiskCache,
    current_memory_size: usize,
}

impl Cache {
    pub async fn new(capacity: usize, max_age: Duration) -> io::Result<Self> {
        Ok(Self {
            store: LruCache::new(NonZeroUsize::new(capacity).unwrap()),
            max_age,
            disk_cache: DiskCache::new().await?,
            current_memory_size: 0,
        })
    }

    pub async fn get(&mut self, key: &str) -> Option<CacheItem> {
        // 先检查内存缓存
        if let Some(item) = self.store.get(key).cloned() {
            if SystemTime::now() < item.expires_at {
                return Some(item);
            }
            self.store.pop(key);
            self.current_memory_size = self.current_memory_size.saturating_sub(item.size);
        }

        // 如果内存中没有，检查磁盘缓存
        if let Ok(Some((data, content_type))) = self.disk_cache.load(key).await {
            let size = data.len();
            let item = CacheItem {
                data,
                content_type,
                expires_at: SystemTime::now() + self.max_age,
                size,
            };
            // 如果数据不太大，放入内存缓存
            if size <= MAX_MEMORY_CACHE_SIZE {
                self.put(key.to_string(), item.clone()).await;
            }
            return Some(item);
        }

        None
    }

    pub async fn put(&mut self, key: String, item: CacheItem) {
        // 如果数据大小超过阈值，直接存入磁盘
        if item.size >= MAX_MEMORY_CACHE_SIZE {
            if let Err(e) = self.disk_cache.save(&key, &item.data, &item.content_type).await {
                eprintln!("保存到磁盘缓存失败: {}", e);
            }
            return;
        }

        // 更新内存使用量
        self.current_memory_size = self.current_memory_size.saturating_add(item.size);

        // 如果内存使用量超过限制，移除旧项直到低于限制
        while self.current_memory_size > MAX_MEMORY_CACHE_SIZE {
            if let Some((_, removed_item)) = self.store.pop_lru() {
                self.current_memory_size = self.current_memory_size.saturating_sub(removed_item.size);
            } else {
                break;
            }
        }

        self.store.put(key.clone(), item.clone());

        // 同时保存到磁盘
        if let Err(e) = self.disk_cache.save(&key, &item.data, &item.content_type).await {
            eprintln!("保存到磁盘缓存失败: {}", e);
        }
    }
}
