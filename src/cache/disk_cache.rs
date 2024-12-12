use bytes::Bytes;
use sha2::{Sha256, Digest};
use std::path::PathBuf;
use tokio::fs;
use std::io;
use super::CACHE_DIR;

pub struct DiskCache {
    cache_dir: PathBuf,
}

impl DiskCache {
    pub async fn new() -> io::Result<Self> {
        let cache_dir = PathBuf::from(CACHE_DIR);
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir).await?;
        }
        Ok(Self { cache_dir })
    }

    fn get_cache_path(&self, key: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let hash = hex::encode(hasher.finalize());
        self.cache_dir.join(hash)
    }

    pub async fn save(&self, key: &str, data: &[u8], content_type: &str) -> io::Result<()> {
        let cache_path = self.get_cache_path(key);
        // 保存内容
        fs::write(&cache_path, data).await?;
        // 保存元数据
        let meta_path = cache_path.with_extension("meta");
        fs::write(meta_path, content_type).await?;
        Ok(())
    }

    pub async fn load(&self, key: &str) -> io::Result<Option<(Bytes, String)>> {
        let cache_path = self.get_cache_path(key);
        if !cache_path.exists() {
            return Ok(None);
        }

        // 读取内容
        let data = fs::read(&cache_path).await?;
        // 读取元数据
        let meta_path = cache_path.with_extension("meta");
        let content_type = fs::read_to_string(meta_path).await?;

        Ok(Some((Bytes::from(data), content_type)))
    }
}
