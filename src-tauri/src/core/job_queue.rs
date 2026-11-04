use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use log;
use tauri::Emitter;

use crate::core::cache::CacheManager;
use crate::core::db::Database;
use crate::core::error::{AppError, AppResult};
use crate::core::scanner::Scanner;
use crate::models::{Job, JobStatus, JobType, LibraryStatus, ReconnectLibraryResult, ScanProgress};

pub trait ProgressUpdate: Send + Sync {
    fn report_progress(&self, progress: f64, processed: i64, total: i64, errors: i64);
    fn report_step(&self, step: String);
    fn report_total(&self, total: i64);
}

pub struct JobControl {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

impl JobControl {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod job_control_tests {
    use super::JobControl;

    #[test]
    fn pause_resume_and_cancel_are_observable() {
        let control = JobControl::new();
        control.pause();
        assert!(control.is_paused());
        control.resume();
        assert!(!control.is_paused());
        control.cancel();
        assert!(control.is_cancelled());
    }
}

pub struct JobQueue {
    db: Arc<Database>,
    cache: Arc<CacheManager>,
    controls: Arc<Mutex<HashMap<String, JobControl>>>,
    running: Arc<AtomicBool>,
    app_handle: tauri::AppHandle,
}

impl JobQueue {
    pub fn new(db: Arc<Database>, cache: Arc<CacheManager>, app_handle: tauri::AppHandle) -> Self {
        Self {
            db,
            cache,
            controls: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            app_handle,
        }
    }

    pub fn submit(&self, job: Job) -> AppResult<Job> {
        let _controls = self.controls.lock().unwrap();
        self.submit_locked(job)
    }

    pub fn set_library_watch_enabled(&self, library_id: &str, enabled: bool) -> AppResult<()> {
        let _controls = self.controls.lock().unwrap();
        self.db.set_library_watch_enabled(library_id, enabled)
    }

    pub fn reconnect_library(
        &self,
        library_id: &str,
        root: &std::path::Path,
    ) -> AppResult<ReconnectLibraryResult> {
        let _controls = self.controls.lock().unwrap();
        let (library, rebased_assets, offline_assets) =
            self.db.reconnect_library_root(library_id, root)?;
        let job = self.submit_locked(Job::scan(library.id.clone(), "等待扫描", 10))?;
        Ok(ReconnectLibraryResult {
            library,
            job,
            rebased_assets,
            offline_assets,
        })
    }

    pub(super) fn submit_watched_scan(
        &self,
        library_id: &str,
        expected_root: &str,
    ) -> AppResult<bool> {
        let _controls = self.controls.lock().unwrap();
        let Some(library) = self.db.get_library(library_id)? else {
            return Ok(true);
        };
        if !library.watch_enabled || library.root_path != expected_root {
            return Ok(true);
        }
        if self.db.active_scan_job_for_library(library_id)?.is_some() {
            return Ok(false);
        }
        self.submit_locked(Job::scan(library.id, "等待自动增量扫描", 0))?;
        Ok(true)
    }

    fn submit_locked(&self, job: Job) -> AppResult<Job> {
        if let Some(library_id) = job
            .library_id
            .as_deref()
            .filter(|_| job.job_type == JobType::Scan)
        {
            if let Some(existing) = self.db.active_scan_job_for_library(library_id)? {
                return Err(AppError::Other(format!(
                    "素材库已有扫描任务（{}）",
                    existing.id
                )));
            }
        }
        self.db.create_job(&job)?;
        self.start_loop();
        Ok(job)
    }

    pub fn pause_job(&self, job_id: &str) -> AppResult<Job> {
        let controls = self.controls.lock().unwrap();
        let mut job = self
            .db
            .get_job(job_id)?
            .ok_or_else(|| AppError::JobNotFound(job_id.to_string()))?;
        if job.status == JobStatus::Running {
            job.status = JobStatus::Paused;
            job.updated_at = Utc::now().timestamp_millis();
            self.db.update_job(&job)?;
            if let Some(control) = controls.get(job_id) {
                control.pause();
            }
            if let Some(library_id) = &job.library_id {
                self.db
                    .update_library_status(library_id, LibraryStatus::Paused, None)?;
            }
        }
        Ok(job)
    }

    pub fn resume_job(&self, job_id: &str) -> AppResult<Job> {
        let controls = self.controls.lock().unwrap();
        let mut job = self
            .db
            .get_job(job_id)?
            .ok_or_else(|| AppError::JobNotFound(job_id.to_string()))?;
        if job.status == JobStatus::Paused {
            let control = controls.get(job_id);
            let library_status = if control.is_some() {
                job.status = JobStatus::Running;
                LibraryStatus::Scanning
            } else {
                job.status = JobStatus::Pending;
                LibraryStatus::Idle
            };
            job.updated_at = Utc::now().timestamp_millis();
            self.db.update_job(&job)?;
            if let Some(control) = control {
                control.resume();
            }
            if let Some(library_id) = &job.library_id {
                self.db
                    .update_library_status(library_id, library_status, None)?;
            }
        }
        self.start_loop();
        Ok(job)
    }

    pub fn cancel_job(&self, job_id: &str) -> AppResult<Job> {
        let controls = self.controls.lock().unwrap();
        let mut job = self
            .db
            .get_job(job_id)?
            .ok_or_else(|| AppError::JobNotFound(job_id.to_string()))?;
        if matches!(
            job.status,
            JobStatus::Pending | JobStatus::Running | JobStatus::Paused
        ) {
            job.status = JobStatus::Cancelled;
            job.finished_at = Some(Utc::now().timestamp_millis());
            job.updated_at = Utc::now().timestamp_millis();
            self.db.update_job(&job)?;
            if let Some(control) = controls.get(job_id) {
                control.cancel();
            }
            if let Some(library_id) = &job.library_id {
                self.db
                    .update_library_status(library_id, LibraryStatus::Idle, None)?;
            }
        }
        Ok(job)
    }

    pub fn retry_job(&self, job_id: &str) -> AppResult<Job> {
        let failed_job = self
            .db
            .get_job(job_id)?
            .ok_or_else(|| AppError::JobNotFound(job_id.to_string()))?;
        if !matches!(failed_job.status, JobStatus::Failed | JobStatus::Cancelled) {
            return Err(AppError::Other(
                "只有失败或已取消的任务可以重试".to_string(),
            ));
        }
        let now = Utc::now().timestamp_millis();
        let retried_job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            status: JobStatus::Pending,
            progress: 0.0,
            current_step: "等待重试".to_string(),
            checkpoint_json: None,
            error_code: None,
            error_message: None,
            started_at: None,
            finished_at: None,
            created_at: now,
            updated_at: now,
            ..failed_job
        };
        self.submit(retried_job.clone())?;
        Ok(retried_job)
    }

    pub fn recover(&self) -> AppResult<()> {
        for job in self.db.recover_interrupted_jobs()? {
            if let Some(library_id) = &job.library_id {
                self.db
                    .update_library_status(library_id, LibraryStatus::Idle, None)?;
            }
        }
        self.start_loop();
        Ok(())
    }

    fn start_loop(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let db = Arc::clone(&self.db);
        let cache = Arc::clone(&self.cache);
        let app = self.app_handle.clone();
        let running = Arc::clone(&self.running);
        let controls = Arc::clone(&self.controls);

        thread::spawn(move || {
            loop {
                // Coordinate worker shutdown and job claims with submit/resume/cancel.
                let mut active_controls = controls.lock().unwrap();
                let next = {
                    match db.list_active_jobs() {
                        Ok(mut jobs) => {
                            jobs.sort_by(|a, b| {
                                b.priority
                                    .cmp(&a.priority)
                                    .then_with(|| a.created_at.cmp(&b.created_at))
                            });
                            jobs.into_iter().next()
                        }
                        Err(e) => {
                            log::error!("读取任务队列失败: {:?}", e);
                            None
                        }
                    }
                };

                let Some(mut job) = next else {
                    running.store(false, Ordering::SeqCst);
                    break;
                };

                let control = JobControl::new();
                job.status = JobStatus::Running;
                job.started_at = Some(Utc::now().timestamp_millis());
                job.updated_at = Utc::now().timestamp_millis();
                if let Err(e) = db.update_job(&job) {
                    log::error!("更新任务状态失败: {:?}", e);
                    running.store(false, Ordering::SeqCst);
                    break;
                }
                active_controls.insert(
                    job.id.clone(),
                    JobControl {
                        cancelled: control.cancelled.clone(),
                        paused: control.paused.clone(),
                    },
                );
                if let Some(library_id) = job
                    .library_id
                    .as_deref()
                    .filter(|_| job.job_type == JobType::Scan)
                {
                    if let Err(e) =
                        db.update_library_status(library_id, LibraryStatus::Scanning, None)
                    {
                        log::error!("更新素材库状态失败: {:?}", e);
                    }
                }
                drop(active_controls);

                let progress_emitter = TauriProgressEmitter {
                    app: app.clone(),
                    db: Arc::clone(&db),
                    controls: Arc::clone(&controls),
                    job_id: job.id.clone(),
                    library_id: job.library_id.clone().unwrap_or_default(),
                };

                let result = match job.job_type {
                    JobType::Scan => run_scan_job(&db, &cache, &job, &control, &progress_emitter),
                    _ => {
                        // 其他类型任务暂按完成处理
                        Ok(())
                    }
                };

                let mut active_controls = controls.lock().unwrap();
                let mut final_job = db
                    .get_job(&job.id)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| job.clone());

                let result = if control.is_cancelled() {
                    Err(AppError::Cancelled)
                } else {
                    result
                };
                match result {
                    Ok(_) => {
                        final_job.status = JobStatus::Completed;
                        final_job.progress = 1.0;
                        final_job.finished_at = Some(Utc::now().timestamp_millis());
                        if let Some(library_id) = &final_job.library_id {
                            let _ = db.update_library_status(
                                library_id,
                                LibraryStatus::Idle,
                                Some(Utc::now().timestamp_millis()),
                            );
                        }
                    }
                    Err(AppError::Cancelled) => {
                        final_job.status = JobStatus::Cancelled;
                        final_job.finished_at = Some(Utc::now().timestamp_millis());
                        if let Some(library_id) = &final_job.library_id {
                            let _ = db.update_library_status(library_id, LibraryStatus::Idle, None);
                        }
                    }
                    Err(AppError::Paused) => {
                        final_job.status = JobStatus::Paused;
                        if let Some(library_id) = &final_job.library_id {
                            let _ =
                                db.update_library_status(library_id, LibraryStatus::Paused, None);
                        }
                    }
                    Err(e) => {
                        final_job.status = JobStatus::Failed;
                        final_job.error_code = Some("scan_failed".to_string());
                        final_job.error_message = Some(e.to_string());
                        final_job.finished_at = Some(Utc::now().timestamp_millis());
                        if let Some(library_id) = &final_job.library_id {
                            let _ =
                                db.update_library_status(library_id, LibraryStatus::Failed, None);
                        }
                    }
                }
                final_job.updated_at = Utc::now().timestamp_millis();
                if let Err(e) = db.update_job(&final_job) {
                    log::error!("持久化最终任务状态失败: {:?}", e);
                }

                active_controls.remove(&final_job.id);

                let metrics = scan_metrics(&final_job.checkpoint_json);
                let _ = progress_emitter.app.emit(
                    "scan:progress",
                    ScanProgress {
                        job_id: final_job.id.clone(),
                        library_id: progress_emitter.library_id.clone(),
                        status: final_job.status,
                        progress: final_job.progress,
                        current_step: final_job.current_step.clone(),
                        processed: metrics.processed,
                        total: metrics.total,
                        errors: metrics.errors,
                    },
                );
                drop(active_controls);

                thread::sleep(Duration::from_millis(100));
            }
        });
    }
}

fn run_scan_job(
    db: &Database,
    cache: &CacheManager,
    job: &Job,
    control: &JobControl,
    progress: &dyn ProgressUpdate,
) -> AppResult<()> {
    let library_id = job
        .library_id
        .as_ref()
        .ok_or_else(|| AppError::Other("扫描任务缺少素材库 ID".to_string()))?;
    let library = db
        .get_library(library_id)?
        .ok_or_else(|| AppError::LibraryNotFound(library_id.clone()))?;

    let scanner = Scanner::new(Arc::new(db.clone()), Arc::new(cache.clone()));
    scanner.scan_library(&library, control, progress)?;
    Ok(())
}

struct TauriProgressEmitter {
    app: tauri::AppHandle,
    db: Arc<Database>,
    controls: Arc<Mutex<HashMap<String, JobControl>>>,
    job_id: String,
    library_id: String,
}

impl ProgressUpdate for TauriProgressEmitter {
    fn report_progress(&self, progress: f64, processed: i64, total: i64, errors: i64) {
        self.publish(
            Some(progress),
            None,
            Some(ScanMetrics {
                processed,
                total,
                errors,
            }),
        );
    }

    fn report_step(&self, step: String) {
        self.publish(None, Some(&step), None);
    }

    fn report_total(&self, total: i64) {
        let step = format!("发现 {} 个文件", total);
        self.publish(
            Some(0.0),
            Some(&step),
            Some(ScanMetrics {
                processed: 0,
                total,
                errors: 0,
            }),
        );
    }
}

impl TauriProgressEmitter {
    fn publish(&self, progress: Option<f64>, step: Option<&str>, metrics: Option<ScanMetrics>) {
        let _controls = self.controls.lock().unwrap();
        let Ok(Some(mut job)) = self.db.get_job(&self.job_id) else {
            return;
        };
        if job.status == JobStatus::Cancelled {
            return;
        }
        if let Some(progress) = progress {
            job.progress = progress.clamp(0.0, 1.0);
        }
        if let Some(step) = step {
            job.current_step = step.to_string();
        }
        if let Some(metrics) = metrics {
            job.checkpoint_json = serde_json::to_string(&metrics).ok();
        }
        job.updated_at = Utc::now().timestamp_millis();
        if let Err(error) = self.db.update_job(&job) {
            log::warn!("持久化任务进度失败: {error}");
        }
        let metrics = scan_metrics(&job.checkpoint_json);
        let _ = self.app.emit(
            "scan:progress",
            ScanProgress {
                job_id: self.job_id.clone(),
                library_id: self.library_id.clone(),
                status: job.status,
                progress: job.progress,
                current_step: job.current_step,
                processed: metrics.processed,
                total: metrics.total,
                errors: metrics.errors,
            },
        );
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ScanMetrics {
    processed: i64,
    total: i64,
    errors: i64,
}

fn scan_metrics(checkpoint: &Option<String>) -> ScanMetrics {
    checkpoint
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default()
}
