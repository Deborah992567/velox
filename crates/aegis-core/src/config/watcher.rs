//! File-based configuration hot-reload watcher.
//!
//! Monitors config files for changes and triggers reload callbacks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Snapshot of a watched file's state.
#[derive(Debug, Clone)]
struct WatchedFile {
    last_modified: SystemTime,
    last_size: u64,
}

/// Configuration reload policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadPolicy {
    /// Reload immediately on change detection.
    Immediate,
    /// Batch changes and reload after a debounce window.
    #[default]
    Debounced,
    /// Only reload on explicit signal (SIGHUP-style).
    Manual,
}

/// Outcome of a single poll cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadEvent {
    /// No files changed since last poll.
    NoChange,
    /// One or more files changed; paths listed.
    Changed(Vec<PathBuf>),
    /// A watched file was deleted.
    Deleted(PathBuf),
}

/// Watches one or more config files for changes.
#[derive(Debug)]
pub struct ConfigWatcher {
    files: HashMap<PathBuf, WatchedFile>,
    poll_interval: Duration,
    debounce_window: Duration,
    policy: ReloadPolicy,
    last_reload: Option<SystemTime>,
}

impl ConfigWatcher {
    pub fn new(policy: ReloadPolicy) -> Self {
        Self {
            files: HashMap::new(),
            poll_interval: Duration::from_secs(1),
            debounce_window: Duration::from_secs(2),
            policy,
            last_reload: None,
        }
    }

    #[must_use]
    pub const fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    #[must_use]
    pub const fn with_debounce_window(mut self, window: Duration) -> Self {
        self.debounce_window = window;
        self
    }

    pub const fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub fn watch(&mut self, path: impl Into<PathBuf>) -> std::io::Result<()> {
        let path = path.into();
        let meta = std::fs::metadata(&path)?;
        self.files.insert(
            path,
            WatchedFile {
                last_modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                last_size: meta.len(),
            },
        );
        Ok(())
    }

    pub fn unwatch(&mut self, path: &Path) -> bool {
        self.files.remove(path).is_some()
    }

    pub fn watched_count(&self) -> usize {
        self.files.len()
    }

    pub fn poll(&mut self) -> ReloadEvent {
        let mut changed = Vec::new();

        let paths: Vec<PathBuf> = self.files.keys().cloned().collect();
        for path in paths {
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    let size = meta.len();
                    if let Some(entry) = self.files.get_mut(&path)
                        && (modified != entry.last_modified || size != entry.last_size)
                    {
                        entry.last_modified = modified;
                        entry.last_size = size;
                        changed.push(path);
                    }
                }
                Err(_) => {
                    return ReloadEvent::Deleted(path);
                }
            }
        }

        if changed.is_empty() {
            ReloadEvent::NoChange
        } else {
            ReloadEvent::Changed(changed)
        }
    }

    pub fn should_reload(&mut self) -> bool {
        if self.policy == ReloadPolicy::Manual {
            return false;
        }

        match self.poll() {
            ReloadEvent::NoChange => false,
            ReloadEvent::Changed(_) => match self.policy {
                ReloadPolicy::Immediate => true,
                ReloadPolicy::Debounced => {
                    let now = SystemTime::now();
                    if let Some(last) = self.last_reload
                        && now.duration_since(last).unwrap_or_default() < self.debounce_window
                    {
                        return false;
                    }
                    self.last_reload = Some(now);
                    true
                }
                ReloadPolicy::Manual => unreachable!(),
            },
            ReloadEvent::Deleted(_) => true,
        }
    }
}

/// Metadata about a reload cycle.
#[derive(Debug, Clone)]
pub struct ReloadInfo {
    pub files_changed: usize,
    pub duration: Duration,
    pub success: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_config() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.conf");
        fs::write(&path, "server { listen 80; }").unwrap();
        (dir, path)
    }

    #[test]
    fn watch_and_detect_change() {
        let (_dir, path) = tmp_config();
        let mut watcher = ConfigWatcher::new(ReloadPolicy::Immediate);
        watcher.watch(&path).unwrap();
        assert_eq!(watcher.watched_count(), 1);
        assert_eq!(watcher.poll(), ReloadEvent::NoChange);
        fs::write(&path, "server { listen 443; }").unwrap();
        assert!(matches!(watcher.poll(), ReloadEvent::Changed(_)));
    }

    #[test]
    fn unwatch_removes_path() {
        let (_dir, path) = tmp_config();
        let mut watcher = ConfigWatcher::new(ReloadPolicy::Manual);
        watcher.watch(&path).unwrap();
        assert!(watcher.unwatch(&path));
        assert!(!watcher.unwatch(&path));
        assert_eq!(watcher.watched_count(), 0);
    }

    #[test]
    fn manual_policy_never_auto_reloads() {
        let (_dir, path) = tmp_config();
        let mut watcher = ConfigWatcher::new(ReloadPolicy::Manual);
        watcher.watch(&path).unwrap();
        fs::write(&path, "changed").unwrap();
        assert!(!watcher.should_reload());
    }

    #[test]
    fn immediate_policy_reloads() {
        let (_dir, path) = tmp_config();
        let mut watcher = ConfigWatcher::new(ReloadPolicy::Immediate);
        watcher.watch(&path).unwrap();
        fs::write(&path, "changed").unwrap();
        assert!(watcher.should_reload());
    }

    #[test]
    fn debounced_policy_throttles() {
        let (_dir, path) = tmp_config();
        let mut watcher = ConfigWatcher::new(ReloadPolicy::Debounced)
            .with_debounce_window(Duration::from_mins(1));
        watcher.watch(&path).unwrap();
        fs::write(&path, "v1").unwrap();
        assert!(watcher.should_reload());
        fs::write(&path, "v2").unwrap();
        assert!(!watcher.should_reload());
    }

    #[test]
    fn deleted_file_detected() {
        let (_dir, path) = tmp_config();
        let mut watcher = ConfigWatcher::new(ReloadPolicy::Immediate);
        watcher.watch(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(watcher.poll(), ReloadEvent::Deleted(path));
    }

    #[test]
    fn default_policy_is_debounced() {
        assert_eq!(ReloadPolicy::default(), ReloadPolicy::Debounced);
    }

    #[test]
    fn poll_interval_configurable() {
        let watcher =
            ConfigWatcher::new(ReloadPolicy::Immediate).with_poll_interval(Duration::from_secs(5));
        assert_eq!(watcher.poll_interval(), Duration::from_secs(5));
    }
}
