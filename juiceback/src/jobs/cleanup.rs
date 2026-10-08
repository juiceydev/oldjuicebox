//! Background job that removes expired files and their metadata.

use std::sync::Arc;
use std::time::Duration;

use crate::db;
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use tokio::sync::Semaphore;

const MINT_LIMITER_PRUNE_INTERVAL_SECS: u64 = 120;

/// Run the cleanup loop forever
pub async fn run_cleanup_loop(state: Arc<AppState>) {
    let interval = Duration::from_secs(state.config.cleanup_interval_minutes * 60);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        // Catch panics so one failed cleanup cycle doesn't kill the loop forever.
        let state_clone = Arc::clone(&state);
        let result = tokio::spawn(async move { run_cleanup_once(&state_clone).await }).await;
        if let Err(e) = result {
            if e.is_panic() {
                tracing::error!(
                    "cleanup: run_cleanup_once panicked, will retry next cycle: {}",
                    e
                );
            } else {
                tracing::error!("cleanup: run_cleanup_once task cancelled: {}", e);
            }
        }
    }
}

async fn run_cleanup_once(state: &Arc<AppState>) {
    tracing::info!("cleanup: starting");

    let now = Utc::now().timestamp();
    let report_cutoff = now - state.config.report_retention_days as i64 * 86_400;
    let feedback_cutoff = now - state.config.feedback_retention_days as i64 * 86_400;
    match state
        .db_call("cleanup_ephemeral_data", move |db| {
            db::cleanup_ephemeral_data(db, now, report_cutoff, feedback_cutoff)
        })
        .await
    {
        Ok(count) if count > 0 => tracing::info!("cleanup: purged {} ephemeral rows", count),
        Err(error) => tracing::error!("cleanup: ephemeral data purge failed: {:?}", error),
        _ => {}
    }

    let state2 = Arc::clone(state);
    let expired =
        match tokio::task::spawn_blocking(move || -> Result<Vec<db::FileRecord>, AppError> {
            let db = state2
                .db
                .get()
                .map_err(|e| AppError::DbPoolError(e.to_string()))?;
            db::list_expired(&db).map_err(AppError::DatabaseError)
        })
        .await
        {
            Ok(Ok(records)) => records,
            Ok(Err(e)) => {
                tracing::error!("cleanup: db query failed: {:?}", e);
                return;
            }
            Err(e) => {
                tracing::error!("cleanup: spawn_blocking panicked: {}", e);
                return;
            }
        };

    if !expired.is_empty() {
        tracing::info!("cleanup: found {} expired files", expired.len());
    }

    // Keep metadata for storage failures so a later cleanup can retry them.
    let sem = Arc::new(Semaphore::new(20));
    let mut pending = FuturesUnordered::new();
    for record in expired {
        let permit = match Arc::clone(&sem).acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::error!("cleanup: semaphore closed, aborting deletion loop");
                break;
            }
        };
        let state = Arc::clone(state);
        let id = record.id.clone();
        let host = record.storage_host.clone();
        let capability = record.delete_token.clone();
        pending.push(tokio::spawn(async move {
            let _permit = permit;
            match crate::juicehost::delete_file_on_juicehost(
                &state,
                &id,
                host.as_deref(),
                Some(&capability),
            )
            .await
            {
                Ok(()) => Some(record),
                Err(e) => {
                    tracing::warn!("cleanup: failed to delete id={} from juicehost: {}", id, e);
                    None
                }
            }
        }));
    }
    let mut deleted = Vec::new();
    while let Some(result) = pending.next().await {
        match result {
            Ok(Some(record)) => deleted.push(record),
            Ok(None) => {}
            Err(e) => tracing::warn!("cleanup: juicehost delete task failed: {}", e),
        }
    }

    let ids_to_purge: Vec<String> = deleted.iter().map(|r| r.id.clone()).collect();

    let cf_urls: Vec<String> = deleted
        .iter()
        .map(|r| {
            crate::utils::public_url(
                &state.config.public_base_url,
                &r.storage_host,
                &r.id,
                &r.filename,
            )
        })
        .collect();
    if !cf_urls.is_empty() {
        crate::cloudflare::purge_urls(Arc::clone(&state.config), cf_urls).await;
        let tags: Vec<String> = deleted
            .iter()
            .map(|r| format!("juicebox:{}", r.id))
            .collect();
        crate::cloudflare::purge_tags(Arc::clone(&state.config), tags).await;
    }

    let state3 = Arc::clone(state);
    let ids_for_purge = ids_to_purge.clone();
    let purge_result = tokio::task::spawn_blocking(move || -> Result<usize, AppError> {
        let db = state3
            .db
            .get()
            .map_err(|e| AppError::DbPoolError(e.to_string()))?;
        db::delete_files_by_ids(&db, &ids_for_purge).map_err(AppError::DatabaseError)
    })
    .await
    .map_err(|e| tracing::error!("cleanup: purge task panicked: {}", e));

    match purge_result {
        Ok(Ok(count)) => tracing::info!("cleanup: purged {} rows from db", count),
        Ok(Err(e)) => tracing::error!("cleanup: failed to purge db rows: {:?}", e),
        Err(_) => {}
    }

    // Remove abandoned TUS uploads older than MIN_TTL_SECONDS.
    // These are uploads that were created but never completed.
    let stale_ids: Vec<String> = state
        .tus
        .iter()
        .filter(|e| now - e.created_at > crate::constants::STALE_UPLOAD_TIMEOUT_SECS)
        .map(|e| e.id.clone())
        .collect();
    for id in &stale_ids {
        if let Some((_, upload)) = state.tus.remove(id) {
            tracing::info!("cleanup: removing abandoned upload id={}", id);
            drop(upload);
        }
        state.tus_senders.remove(id);
        state.push_handles.remove(id);
    }
    // Remove stale part sessions (orphaned by abandoned uploads).
    let stale_sessions: Vec<String> = state
        .part_sessions
        .iter()
        .filter(|e| {
            // A session is stale if its oldest part is older than MIN_TTL_SECONDS.
            // We don't have a timestamp on PartSession, so just check if any
            // of its parts are still in the TUS map. If not, it's orphaned.
            let part_ids: Vec<String> = e
                .value()
                .part_ids
                .iter()
                .map(|p| p.value().clone())
                .collect();
            part_ids.iter().all(|pid| !state.tus.contains_key(pid))
        })
        .map(|e| e.key().clone())
        .collect();
    for sid in &stale_sessions {
        state.part_sessions.remove(sid);
    }

    // Clean up abandoned reserved uploads (status='uploading', size_bytes=0).
    // These are Quick Link reservations where the client never completed the upload.
    // No bytes were stored on juicehost, so only the database records need removal.
    let state4 = Arc::clone(state);
    let abandoned =
        match tokio::task::spawn_blocking(move || -> Result<Vec<db::FileRecord>, AppError> {
            let db = state4
                .db
                .get()
                .map_err(|e| AppError::DbPoolError(e.to_string()))?;
            db::list_abandoned_uploads(&db, crate::constants::STALE_UPLOAD_TIMEOUT_SECS)
                .map_err(AppError::DatabaseError)
        })
        .await
        {
            Ok(Ok(records)) => records,
            Ok(Err(e)) => {
                tracing::error!("cleanup: abandoned uploads query failed: {:?}", e);
                Vec::new()
            }
            Err(e) => {
                tracing::error!("cleanup: abandoned uploads spawn_blocking panicked: {}", e);
                Vec::new()
            }
        };

    if !abandoned.is_empty() {
        tracing::info!(
            "cleanup: found {} abandoned uploads (0B, never completed)",
            abandoned.len()
        );
        let ids_to_purge: Vec<String> = abandoned.iter().map(|r| r.id.clone()).collect();
        let state5 = Arc::clone(state);
        let purge_result = tokio::task::spawn_blocking(move || -> Result<usize, AppError> {
            let db = state5
                .db
                .get()
                .map_err(|e| AppError::DbPoolError(e.to_string()))?;
            db::delete_files_by_ids(&db, &ids_to_purge).map_err(AppError::DatabaseError)
        })
        .await;

        match purge_result {
            Ok(Ok(count)) => tracing::info!("cleanup: purged {} abandoned upload records", count),
            Ok(Err(e)) => tracing::error!("cleanup: failed to purge abandoned uploads: {:?}", e),
            Err(e) => tracing::error!("cleanup: abandoned uploads purge task panicked: {}", e),
        }
    }

    if !stale_ids.is_empty() || !stale_sessions.is_empty() {
        tracing::info!(
            "cleanup: removed {} abandoned uploads, {} stale sessions",
            stale_ids.len(),
            stale_sessions.len()
        );
    }
}

pub async fn run_mint_limiter_prune_loop(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(MINT_LIMITER_PRUNE_INTERVAL_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let before = state.mint_limiter.len_probe();
        state.mint_limiter.prune();
        if before != 0 {
            tracing::debug!("mint limiter: pruned buckets before={}", before);
        }
    }
}
