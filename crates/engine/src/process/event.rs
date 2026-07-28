//! Ordered process events and bounded diagnostic output.

/// The standard stream that produced output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// One retained output chunk in the order observed by the process owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessEvent {
    /// Monotonic sequence number for this process.
    pub sequence: u64,
    /// Stream that produced the bytes.
    pub stream: OutputStream,
    /// Exact bytes retained from the stream.
    pub bytes: Vec<u8>,
}

/// Bounded bytes and counters for one standard stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamCapture {
    /// Exact retained bytes.
    pub bytes: Vec<u8>,
    /// Total bytes observed before the pipe closed.
    pub observed_bytes: u64,
    /// Total logical lines observed before the pipe closed.
    pub observed_lines: u64,
    /// Whether any bytes were discarded because of a configured limit.
    pub truncated: bool,
}

/// Bounded stdout, stderr, and their observed interleaving.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapturedOutput {
    /// Standard output capture.
    pub stdout: StreamCapture,
    /// Standard error capture.
    pub stderr: StreamCapture,
    /// Retained chunks in observation order.
    pub events: Vec<ProcessEvent>,
}
