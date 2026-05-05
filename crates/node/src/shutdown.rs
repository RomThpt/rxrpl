//! Process-level shutdown signal handling.
//!
//! Resolves on the first of SIGINT or SIGTERM (Unix) so that operators
//! using `systemctl stop` (which sends SIGTERM) get the same graceful
//! shutdown as interactive Ctrl+C. On non-Unix targets we fall back to
//! `tokio::signal::ctrl_c`.

use std::io;

/// Wait for SIGINT or SIGTERM. Returns the canonical signal name that
/// triggered the wake so callers can log it.
pub async fn wait_for_shutdown() -> io::Result<&'static str> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = sigint.recv() => Ok("SIGINT"),
            _ = sigterm.recv() => Ok("SIGTERM"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        Ok("CTRL_C")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Helper installs the SIGINT/SIGTERM listeners without panicking.
    /// Full end-to-end signal delivery is covered by binary-level
    /// integration tests (running `rxrpl run` and sending SIGTERM via
    /// systemd or `kill`) rather than here, because cargo's shared
    /// test process would also receive any signal we raise to self.
    #[tokio::test]
    async fn install_does_not_panic() {
        let handle = tokio::spawn(async {
            tokio::select! {
                r = wait_for_shutdown() => r,
                _ = tokio::time::sleep(Duration::from_millis(50)) => Ok("TIMEOUT"),
            }
        });
        let result = handle.await.unwrap().unwrap();
        assert_eq!(result, "TIMEOUT");
    }
}
