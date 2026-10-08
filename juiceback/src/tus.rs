//! In-memory state for TUS resumable uploads.

use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use tokio::sync::{mpsc, Mutex};

use crate::upload_mode::UploadMode;

/// TUS session whose chunks are streamed to juicehost without disk buffering.
#[derive(Debug)]
pub struct TusUpload {
    pub id: String,
    pub offset: u64,
    pub total_length: u64,
    pub filename: String,
    pub mime_type: String,
    pub ttl_hours: f64,
    pub created_at: i64,
    pub delete_token: String,
    /// HMAC-SHA256 hash of the uploader's IP so we can count sessions per-IP in memory
    pub hashed_ip: String,
    /// AES-256-GCM encrypted IP, for storage in the database.
    pub encrypted_ip: String,
    pub storage_host: Option<String>,
    /// If this upload is part of a parallel session, which session.
    pub session_id: Option<String>,
    /// Part index within the session (0-based).
    pub part_index: Option<usize>,
    /// Quick Link reservation ID: if set, update this existing record on completion.
    pub reserve_id: Option<String>,
    /// Capability for the reservation, supplied as `delete_token` metadata.
    pub reservation_token: Option<String>,
    /// Shared per-file capability used for all parts and the final concat.
    pub capability: String,
    /// Serializes offset validation, streaming, and offset advancement for PATCH requests.
    pub patch_lock: Arc<Mutex<()>>,
    pub user_id: String,
    /// Transport used for the storage push (from upload metadata).
    pub upload_mode: UploadMode,
    /// Chunk receiver handed to the storage push task when the first chunk
    /// arrives; `None` once the push has been spawned.
    pub push_rx: Option<mpsc::Receiver<Result<Bytes, String>>>,
}

/// INVARIANT for every DashMap in this module: never hold a `Ref`/`RefMut`
/// across an `.await`. A guard borrows the shard's parking_lot lock, and
/// parking a runtime worker thread on a contended shard while the guard
/// holder needs that same thread to run is a full accept-starvation
/// deadlock (observed in production: idle CPU, pinned backlog, all workers
/// parked in `DashMap::entry`). Clone the value out (`map(|r| r.clone())`)
/// or scope the guard to a synchronous block instead.
pub type TusMap = Arc<DashMap<String, TusUpload>>;

pub fn new_tus_state() -> TusMap {
    Arc::new(DashMap::new())
}

/// Per-upload mpsc sender for streaming chunks to the push task.
pub type TusSenderMap = Arc<DashMap<String, mpsc::Sender<Result<Bytes, String>>>>;

pub fn new_tus_sender_map() -> TusSenderMap {
    Arc::new(DashMap::new())
}

/// Tracks independent TUS parts that are concatenated after all pushes succeed.
pub struct PartSession {
    pub session_id: String,
    pub total_parts: usize,
    pub filename: String,
    pub mime_type: String,
    pub hashed_ip: String,
    pub storage_host: Option<String>,
    pub ttl_hours: f64,
    pub reserve_id: Option<String>,
    pub reservation_token: Option<String>,
    pub capability: String,
    pub user_id: String,
    pub upload_mode: UploadMode,
    pub declared_size: std::sync::atomic::AtomicU64,
    pub completed: std::sync::atomic::AtomicUsize,
    /// Maps part indices to juicehost file IDs and their declared lengths.
    pub part_ids: DashMap<usize, String>,
    pub part_lengths: DashMap<usize, u64>,
    pub completion_reserve_id: std::sync::Mutex<Option<Option<String>>>,
}

impl std::fmt::Debug for PartSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartSession")
            .field("session_id", &self.session_id)
            .field("total_parts", &self.total_parts)
            .field(
                "completed",
                &self.completed.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

pub type PartSessionMap = Arc<DashMap<String, Arc<PartSession>>>;

pub fn new_part_session_map() -> PartSessionMap {
    Arc::new(DashMap::new())
}
