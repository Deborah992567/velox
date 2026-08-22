//! Master/worker process architecture.
//!
//! Phase 17: Fork-based process model with `SO_REUSEPORT`, worker supervision,
//! privilege drop, and PID file management. Implements ADR 0005.
#![allow(unsafe_code)]

pub mod privilege;
pub mod signals;
pub mod worker;

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Global state shared between master and signal handlers.
#[derive(Debug)]
pub struct MasterState {
    shutdown_requested: AtomicBool,
    reload_requested: AtomicBool,
    active_workers: AtomicUsize,
    pid: u32,
    worker_pids: Mutex<HashMap<u32, WorkerInfo>>,
    start_time: Instant,
    config_path: String,
}

/// Information about a running worker.
#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub pid: u32,
    pub started_at: Instant,
    pub restarts: u32,
}

impl WorkerInfo {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            started_at: Instant::now(),
            restarts: 0,
        }
    }

    fn with_restarts(pid: u32, restarts: u32) -> Self {
        Self {
            pid,
            started_at: Instant::now(),
            restarts,
        }
    }
}

impl MasterState {
    /// Create a new master state.
    pub fn new(config_path: String) -> Self {
        Self {
            shutdown_requested: AtomicBool::new(false),
            reload_requested: AtomicBool::new(false),
            active_workers: AtomicUsize::new(0),
            pid: std::process::id(),
            worker_pids: Mutex::new(HashMap::new()),
            start_time: Instant::now(),
            config_path,
        }
    }

    /// Request graceful shutdown.
    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    /// Whether shutdown has been requested.
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    /// Request a graceful config reload.
    pub fn request_reload(&self) {
        self.reload_requested.store(true, Ordering::Release);
    }

    /// Check and clear the reload flag.
    pub fn take_reload(&self) -> bool {
        self.reload_requested.swap(false, Ordering::AcqRel)
    }

    /// Register a new worker.
    ///
    /// # Panics
    ///
    /// Panics if the internal worker-map mutex is poisoned.
    pub fn register_worker(&self, pid: u32) {
        let mut workers = self.worker_pids.lock().expect("worker lock poisoned");
        workers.insert(pid, WorkerInfo::new(pid));
        drop(workers);
        self.active_workers.fetch_add(1, Ordering::Relaxed);
    }

    /// Unregister a worker (called on waitpid).
    ///
    /// # Panics
    ///
    /// Panics if the internal worker-map mutex is poisoned.
    pub fn unregister_worker(&self, pid: u32) -> Option<WorkerInfo> {
        let mut workers = self.worker_pids.lock().expect("worker lock poisoned");
        let info = workers.remove(&pid);
        drop(workers);
        self.active_workers.fetch_sub(1, Ordering::Relaxed);
        info
    }

    /// Register a restarted worker, preserving restart count.
    ///
    /// # Panics
    ///
    /// Panics if the internal worker-map mutex is poisoned.
    pub fn register_restart(&self, old_pid: u32, new_pid: u32) {
        let mut workers = self.worker_pids.lock().expect("worker lock poisoned");
        let restarts = workers.remove(&old_pid).map_or(0, |info| info.restarts + 1);
        workers.insert(new_pid, WorkerInfo::with_restarts(new_pid, restarts));
        drop(workers);
        // The old worker was already counted; we only replace it, so net 0 change.
    }

    /// Number of active workers.
    pub fn active_workers(&self) -> usize {
        self.active_workers.load(Ordering::Relaxed)
    }

    /// Master PID.
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Server uptime.
    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Config path.
    pub fn config_path(&self) -> &str {
        &self.config_path
    }

    /// Snapshot of all worker PIDs and their info.
    ///
    /// # Panics
    ///
    /// Panics if the internal worker-map mutex is poisoned.
    pub fn worker_snapshot(&self) -> Vec<(u32, WorkerInfo)> {
        let workers = self.worker_pids.lock().expect("worker lock poisoned");
        workers
            .iter()
            .map(|(&pid, info)| (pid, info.clone()))
            .collect()
    }
}

impl Default for MasterState {
    fn default() -> Self {
        Self::new("/etc/aegis/aegis.conf".to_owned())
    }
}

/// PID file management.
#[derive(Debug)]
pub struct PidFile {
    path: std::path::PathBuf,
}

impl PidFile {
    /// Create or overwrite a PID file.
    pub fn create(path: impl Into<std::path::PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(Self { path })
    }

    /// Read the PID from an existing PID file.
    pub fn read(path: impl AsRef<std::path::Path>) -> std::io::Result<u32> {
        let content = std::fs::read_to_string(path.as_ref())?;
        content
            .trim()
            .parse::<u32>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Check if the process in the PID file is still running.
    pub fn is_running(path: impl AsRef<std::path::Path>) -> bool {
        Self::read(path).is_ok_and(process_exists)
    }

    /// Remove the PID file.
    pub fn remove(&self) -> std::io::Result<()> {
        std::fs::remove_file(&self.path)
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

/// Check if a process with the given PID exists.
fn process_exists(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is a standard POSIX diagnostic syscall.
    // It sends no signal but checks if the process exists.
    #[cfg(unix)]
    {
        // pid_t is signed on POSIX but PIDs are always non-negative.
        #[allow(clippy::cast_possible_wrap)]
        unsafe {
            libc::kill(pid as i32, 0) == 0
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_state_basics() {
        let state = MasterState::new("/tmp/test.conf".to_owned());
        assert!(!state.is_shutdown_requested());
        assert!(!state.take_reload());
        assert_eq!(state.active_workers(), 0);
        assert_eq!(state.config_path(), "/tmp/test.conf");
    }

    #[test]
    fn worker_registration() {
        let state = MasterState::new("/tmp/test.conf".to_owned());
        state.register_worker(100);
        state.register_worker(101);
        assert_eq!(state.active_workers(), 2);

        let snapshot = state.worker_snapshot();
        assert_eq!(snapshot.len(), 2);

        state.unregister_worker(100);
        assert_eq!(state.active_workers(), 1);
    }

    #[test]
    fn worker_restart_preserves_count() {
        let state = MasterState::new("/tmp/test.conf".to_owned());
        state.register_worker(100);
        state.register_restart(100, 200);
        assert_eq!(state.active_workers(), 1);

        let snapshot = state.worker_snapshot();
        let (_, info) = snapshot.iter().find(|(pid, _)| *pid == 200).unwrap();
        assert_eq!(info.restarts, 1);
    }

    #[test]
    fn pid_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aegis.pid");
        let pf = PidFile::create(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.trim().parse::<u32>().unwrap(), std::process::id());
        pf.remove().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn shutdown_and_reload() {
        let state = MasterState::new("/tmp/test.conf".to_owned());
        state.request_shutdown();
        assert!(state.is_shutdown_requested());

        state.request_reload();
        assert!(state.take_reload());
        assert!(!state.take_reload());
    }
}
