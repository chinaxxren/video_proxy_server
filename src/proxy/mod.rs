mod server;
mod handler;

pub use server::*;
pub use handler::*;

pub const CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks
