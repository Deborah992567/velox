//! HTTP/2 stream management and flow control (RFC 9113 S5).

use std::collections::HashMap;

/// Stream states (RFC 9113 S5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    Idle,
    ReservedLocal,
    ReservedRemote,
    Open,
    HalfClosedLocal,
    HalfClosedRemote,
    Closed,
}

impl StreamState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// Flow-control window (RFC 9113 S5.2).
#[derive(Debug)]
pub struct FlowWindow {
    pub size: i64,
    pub max_size: i64,
}

impl FlowWindow {
    pub const fn new(initial: i64) -> Self {
        Self {
            size: initial,
            max_size: i64::MAX,
        }
    }

    pub const fn consume(&mut self, n: i64) -> bool {
        if self.size >= n {
            self.size -= n;
            true
        } else {
            false
        }
    }

    pub const fn update(&mut self, increment: i64) -> Result<(), FlowControlError> {
        self.size = self.size.saturating_add(increment);
        if self.size > self.max_size {
            return Err(FlowControlError);
        }
        Ok(())
    }

    pub const fn is_open(&self) -> bool {
        self.size > 0
    }
}

/// Error when flow control window overflows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowControlError;

impl std::fmt::Display for FlowControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "flow control window overflow")
    }
}

impl std::error::Error for FlowControlError {}

/// An HTTP/2 stream.
#[derive(Debug)]
pub struct Stream {
    pub id: u32,
    pub state: StreamState,
    pub send_window: FlowWindow,
    pub recv_window: FlowWindow,
    pub pending_data: Vec<u8>,
    pub headers_complete: bool,
    pub end_of_stream: bool,
}

impl Stream {
    pub const fn new(id: u32, initial_window: i64) -> Self {
        Self {
            id,
            state: StreamState::Idle,
            send_window: FlowWindow::new(initial_window),
            recv_window: FlowWindow::new(initial_window),
            pending_data: Vec::new(),
            headers_complete: false,
            end_of_stream: false,
        }
    }

    pub fn open(&mut self) {
        if self.state == StreamState::Idle {
            self.state = StreamState::Open;
        }
    }

    pub fn half_close_local(&mut self) {
        if self.state == StreamState::Open {
            self.state = StreamState::HalfClosedLocal;
        }
    }

    pub fn half_close_remote(&mut self) {
        if self.state == StreamState::Open {
            self.state = StreamState::HalfClosedRemote;
        }
    }

    pub const fn close(&mut self) {
        self.state = StreamState::Closed;
    }

    #[allow(clippy::cast_possible_wrap)]
    pub fn receive_data(&mut self, data: &[u8]) -> Result<(), FlowControlError> {
        if self.recv_window.consume(data.len() as i64) {
            self.pending_data.extend_from_slice(data);
            Ok(())
        } else {
            Err(FlowControlError)
        }
    }
}

/// Errors during stream operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    TooManyStreams,
    StreamAlreadyExists,
    InvalidStreamId,
    StreamClosed,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyStreams => write!(f, "too many concurrent streams"),
            Self::StreamAlreadyExists => write!(f, "stream already exists"),
            Self::InvalidStreamId => write!(f, "invalid stream ID"),
            Self::StreamClosed => write!(f, "stream is closed"),
        }
    }
}

impl std::error::Error for StreamError {}

/// Stream manager for a connection.
#[derive(Debug)]
pub struct StreamManager {
    streams: HashMap<u32, Stream>,
    max_concurrent: u32,
    active_count: u32,
}

impl StreamManager {
    pub fn new(max_concurrent: u32, _initial_window: i64) -> Self {
        Self {
            streams: HashMap::new(),
            max_concurrent,
            active_count: 0,
        }
    }

    /// # Panics
    ///
    /// Panics if the stream was just inserted (internal invariant).
    pub fn open_stream(
        &mut self,
        id: u32,
        initial_window: i64,
    ) -> Result<&mut Stream, StreamError> {
        if self.active_count >= self.max_concurrent {
            return Err(StreamError::TooManyStreams);
        }
        if self.streams.contains_key(&id) {
            return Err(StreamError::StreamAlreadyExists);
        }
        let mut stream = Stream::new(id, initial_window);
        stream.open();
        self.active_count += 1;
        self.streams.insert(id, stream);
        Ok(self.streams.get_mut(&id).unwrap())
    }

    pub fn get_stream(&self, id: u32) -> Option<&Stream> {
        self.streams.get(&id)
    }

    pub fn get_stream_mut(&mut self, id: u32) -> Option<&mut Stream> {
        self.streams.get_mut(&id)
    }

    pub fn close_stream(&mut self, id: u32) -> Option<Stream> {
        if let Some(mut stream) = self.streams.remove(&id) {
            stream.close();
            self.active_count = self.active_count.saturating_sub(1);
            Some(stream)
        } else {
            None
        }
    }

    pub const fn active_streams(&self) -> u32 {
        self.active_count
    }

    pub const fn is_full(&self) -> bool {
        self.active_count >= self.max_concurrent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_state_transitions() {
        let mut s = Stream::new(1, 65_535);
        assert_eq!(s.state, StreamState::Idle);
        s.open();
        assert_eq!(s.state, StreamState::Open);
        s.half_close_local();
        assert_eq!(s.state, StreamState::HalfClosedLocal);
        s.close();
        assert_eq!(s.state, StreamState::Closed);
        assert!(s.state.is_terminal());
    }

    #[test]
    fn flow_window_consume() {
        let mut w = FlowWindow::new(100);
        assert!(w.consume(50));
        assert_eq!(w.size, 50);
        assert!(!w.consume(60));
        assert_eq!(w.size, 50);
    }

    #[test]
    fn flow_window_update() {
        let mut w = FlowWindow::new(100);
        assert!(w.consume(100));
        assert!(!w.is_open());
        w.update(50).unwrap();
        assert!(w.is_open());
    }

    #[test]
    fn flow_window_overflow() {
        let mut w = FlowWindow::new(100);
        w.max_size = 200;
        assert!(w.update(99).is_ok());
        assert_eq!(w.size, 199);
        assert!(w.update(1).is_ok());
        assert_eq!(w.size, 200);
        assert!(w.update(1).is_err());
    }

    #[test]
    fn stream_manager_concurrent_limit() {
        let mut mgr = StreamManager::new(2, 65_535);
        mgr.open_stream(1, 65_535).unwrap();
        mgr.open_stream(3, 65_535).unwrap();
        assert!(matches!(
            mgr.open_stream(5, 65_535),
            Err(StreamError::TooManyStreams)
        ));
    }

    #[test]
    fn stream_manager_duplicate_id() {
        let mut mgr = StreamManager::new(10, 65_535);
        mgr.open_stream(1, 65_535).unwrap();
        assert!(matches!(
            mgr.open_stream(1, 65_535),
            Err(StreamError::StreamAlreadyExists)
        ));
    }

    #[test]
    fn stream_manager_close_frees_slot() {
        let mut mgr = StreamManager::new(1, 65_535);
        mgr.open_stream(1, 65_535).unwrap();
        mgr.close_stream(1);
        assert_eq!(mgr.active_streams(), 0);
        mgr.open_stream(3, 65_535).unwrap();
    }

    #[test]
    fn stream_receive_data() {
        let mut s = Stream::new(1, 100);
        s.open();
        assert!(s.receive_data(b"hello").is_ok());
        assert_eq!(s.pending_data, b"hello");
    }

    #[test]
    fn stream_receive_data_overflow() {
        let mut s = Stream::new(1, 3);
        s.open();
        assert!(s.receive_data(b"hello").is_err());
    }

    #[test]
    fn idle_to_open_only() {
        let mut s = Stream::new(1, 100);
        s.half_close_local();
        assert_eq!(s.state, StreamState::Idle);
        s.open();
        assert_eq!(s.state, StreamState::Open);
    }

    #[test]
    fn active_count_decrements_on_close() {
        let mut mgr = StreamManager::new(5, 65_535);
        mgr.open_stream(1, 65_535).unwrap();
        mgr.open_stream(3, 65_535).unwrap();
        mgr.close_stream(1);
        assert_eq!(mgr.active_streams(), 1);
        mgr.close_stream(3);
        assert_eq!(mgr.active_streams(), 0);
    }

    #[test]
    fn is_full_reports_correctly() {
        let mut mgr = StreamManager::new(2, 65_535);
        assert!(!mgr.is_full());
        mgr.open_stream(1, 65_535).unwrap();
        assert!(!mgr.is_full());
        mgr.open_stream(3, 65_535).unwrap();
        assert!(mgr.is_full());
    }

    #[test]
    fn get_stream_and_mut() {
        let mut mgr = StreamManager::new(5, 65_535);
        mgr.open_stream(1, 65_535).unwrap();
        assert!(mgr.get_stream(1).is_some());
        assert!(mgr.get_stream(2).is_none());
        mgr.get_stream_mut(1).unwrap().end_of_stream = true;
        assert!(mgr.get_stream(1).unwrap().end_of_stream);
    }
}
