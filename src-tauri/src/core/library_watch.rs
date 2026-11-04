use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use super::db::Database;
use super::job_queue::JobQueue;
use super::scanner::{media_snapshot, MediaSnapshot};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct WatchedLibrary {
    root: String,
    snapshot: Option<MediaSnapshot>,
    needs_scan: bool,
}

pub(super) fn start(db: Arc<Database>, queue: Weak<JobQueue>) {
    std::thread::spawn(move || {
        let mut watched = HashMap::<String, WatchedLibrary>::new();
        loop {
            let Some(queue) = queue.upgrade() else { break };
            match db.list_libraries() {
                Ok(libraries) => {
                    watched.retain(|id, _| {
                        libraries
                            .iter()
                            .any(|library| &library.id == id && library.watch_enabled)
                    });
                    for library in libraries
                        .into_iter()
                        .filter(|library| library.watch_enabled)
                    {
                        let state = watched.entry(library.id.clone()).or_default();
                        if state.root != library.root_path {
                            *state = WatchedLibrary {
                                root: library.root_path.clone(),
                                ..Default::default()
                            };
                        }
                        let Ok(snapshot) = media_snapshot(&library) else {
                            state.snapshot = None;
                            continue;
                        };
                        if state.snapshot.as_ref() != Some(&snapshot) {
                            state.snapshot = Some(snapshot);
                            state.needs_scan = true;
                            continue;
                        }
                        // Wait for two identical observations to group changes and avoid
                        // scanning while a copy is still updating file metadata.
                        if state.needs_scan {
                            match queue.submit_watched_scan(&library.id, &state.root) {
                                Ok(submitted) => state.needs_scan = !submitted,
                                Err(error) => log::warn!("自动扫描入队失败: {error}"),
                            }
                        }
                    }
                }
                Err(error) => log::warn!("读取监听素材库失败: {error}"),
            }
            drop(queue);
            std::thread::sleep(POLL_INTERVAL);
        }
    });
}
