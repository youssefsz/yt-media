//! Cooperative cancellation shared by engine services and process adapters.

/// A cloneable cancellation signal.
///
/// Cancelling any clone wakes all waiters. Child tokens inherit cancellation from their parent
/// without allowing a child to cancel the parent.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(tokio_util::sync::CancellationToken);

impl CancellationToken {
    /// Creates a token in the active state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.cancel();
    }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Creates a token cancelled when either it or this parent is cancelled.
    #[must_use]
    pub fn child_token(&self) -> Self {
        Self(self.0.child_token())
    }

    /// Waits until cancellation is requested.
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}
