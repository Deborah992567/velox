//! WebSocket configuration options.
//!
//! Tuning knobs for WebSocket upgrade behavior, message limits, and timeouts.

use std::time::Duration;

/// Configuration for WebSocket connections.
#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    /// Maximum frame size in bytes.
    pub max_frame_size: usize,
    /// Maximum message size in bytes (after reassembly).
    pub max_message_size: usize,
    /// Timeout for the opening handshake.
    pub handshake_timeout: Duration,
    /// Idle timeout before closing the connection.
    pub idle_timeout: Duration,
    /// Whether to enable per-message compression.
    pub enable_compression: bool,
    /// Maximum number of pending messages in the write buffer.
    pub write_buffer_size: usize,
    /// Maximum number of pending messages in the read buffer.
    pub read_buffer_size: usize,
    /// Maximum send queue before dropping the connection.
    pub max_send_queue: usize,
    /// Whether to send close frame on shutdown.
    pub send_close_on_shutdown: bool,
    /// Maximum duration for a single message write.
    pub write_timeout: Duration,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            max_frame_size: 16 * 1024 * 1024,
            max_message_size: 64 * 1024 * 1024,
            handshake_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_mins(5),
            enable_compression: false,
            write_buffer_size: 128,
            read_buffer_size: 128,
            max_send_queue: 256,
            send_close_on_shutdown: true,
            write_timeout: Duration::from_secs(30),
        }
    }
}

impl WebSocketConfig {
    /// Strict configuration for untrusted clients.
    #[must_use]
    pub const fn strict() -> Self {
        Self {
            max_frame_size: 1024 * 1024,
            max_message_size: 4 * 1024 * 1024,
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_mins(1),
            enable_compression: false,
            write_buffer_size: 32,
            read_buffer_size: 32,
            max_send_queue: 64,
            send_close_on_shutdown: true,
            write_timeout: Duration::from_secs(10),
        }
    }

    /// Relaxed configuration for trusted internal clients.
    #[must_use]
    pub const fn relaxed() -> Self {
        Self {
            max_frame_size: 64 * 1024 * 1024,
            max_message_size: 256 * 1024 * 1024,
            handshake_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_hours(1),
            enable_compression: true,
            write_buffer_size: 512,
            read_buffer_size: 512,
            max_send_queue: 1024,
            send_close_on_shutdown: true,
            write_timeout: Duration::from_mins(1),
        }
    }

    /// Check if a frame size is within the configured limit.
    #[must_use]
    pub const fn is_frame_within_limit(&self, size: usize) -> bool {
        size <= self.max_frame_size
    }

    /// Check if a message size is within the configured limit.
    #[must_use]
    pub const fn is_message_within_limit(&self, size: usize) -> bool {
        size <= self.max_message_size
    }

    /// Check if the send queue is full.
    #[must_use]
    pub const fn is_send_queue_full(&self, pending: usize) -> bool {
        pending >= self.max_send_queue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let c = WebSocketConfig::default();
        assert_eq!(c.max_frame_size, 16 * 1024 * 1024);
        assert_eq!(c.max_message_size, 64 * 1024 * 1024);
        assert!(!c.enable_compression);
    }

    #[test]
    fn strict_config_limits() {
        let c = WebSocketConfig::strict();
        assert!(c.max_frame_size < WebSocketConfig::default().max_frame_size);
        assert!(c.max_message_size < WebSocketConfig::default().max_message_size);
        assert!(c.idle_timeout < Duration::from_mins(2));
    }

    #[test]
    fn relaxed_config_limits() {
        let c = WebSocketConfig::relaxed();
        assert!(c.max_frame_size > WebSocketConfig::default().max_frame_size);
        assert!(c.enable_compression);
    }

    #[test]
    fn frame_limit_check() {
        let c = WebSocketConfig::default();
        assert!(c.is_frame_within_limit(100));
        assert!(!c.is_frame_within_limit(c.max_frame_size + 1));
    }

    #[test]
    fn message_limit_check() {
        let c = WebSocketConfig::default();
        assert!(c.is_message_within_limit(0));
        assert!(!c.is_message_within_limit(c.max_message_size + 1));
    }

    #[test]
    fn send_queue_full() {
        let c = WebSocketConfig::default();
        assert!(!c.is_send_queue_full(0));
        assert!(c.is_send_queue_full(c.max_send_queue));
    }

    #[test]
    fn defaults_reasonable() {
        let c = WebSocketConfig::default();
        assert!(c.handshake_timeout > Duration::ZERO);
        assert!(c.idle_timeout > Duration::ZERO);
        assert!(c.write_buffer_size > 0);
        assert!(c.max_send_queue > 0);
    }
}
