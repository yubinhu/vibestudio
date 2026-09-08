//! Retry policy shared by automatic tunnel recovery and explicit retry.
use std::time::Duration;

pub(super) fn delay(attempt: u32) -> Duration {
    Duration::from_millis((500u64 << attempt.min(6)).min(30_000))
}

/// Configuration, trust and authentication failures need user attention.
/// Unknown failures also stop rather than repeatedly provisioning a host.
pub(super) fn transient(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if [
        "permission denied",
        "authentication",
        "host key",
        "host-key",
        "known_hosts",
        "fingerprint",
        "private key",
        "checksum",
        "no saved connection",
        "incompatible",
    ]
    .iter()
    .any(|part| message.contains(part))
    {
        return false;
    }
    [
        "timed out",
        "timeout",
        "connection refused",
        "connection reset",
        "connection closed",
        "connection lost",
        "connection aborted",
        "connection unexpectedly closed",
        "network",
        "unreachable",
        "no route to host",
        "broken pipe",
        "could not resolve",
        "failed to lookup",
        "name or service not known",
        "temporary failure",
        "unexpected eof",
    ]
    .iter()
    .any(|part| message.contains(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_and_network_failures_retry_but_trust_and_install_failures_do_not() {
        for error in [
            "Connection reset by peer",
            "network unreachable",
            "timed out waiting for remote",
            "Could not resolve hostname workbox",
        ] {
            assert!(transient(error), "{error}");
        }
        for error in [
            "Permission denied (publickey)",
            "SSH host key changed",
            "private key unavailable",
            "download checksum mismatch",
            "incompatible host service",
            "unknown installer failure",
        ] {
            assert!(!transient(error), "{error}");
        }
    }

    #[test]
    fn retry_delay_is_bounded() {
        assert_eq!(delay(0), Duration::from_millis(500));
        assert_eq!(delay(2), Duration::from_secs(2));
        assert_eq!(delay(u32::MAX), Duration::from_secs(30));
    }
}
