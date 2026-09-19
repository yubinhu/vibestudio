//! Shared release floor for local, SSH, WSL and mobile accessors. The service
//! protocol still governs wire compatibility; a matching protocol alone cannot
//! promise that an older worker implements a newer client's API routes.

use semver::{BuildMetadata, Version};

fn parse(value: &str) -> Result<Version, String> {
    let mut version = Version::parse(value).map_err(|_| {
        format!("incompatible host service version {value:?}: a valid release version is required. Update the host service and reconnect.")
    })?;
    // Build labels do not change SemVer precedence.
    version.build = BuildMetadata::EMPTY;
    Ok(version)
}

/// A development client (0.0.0) has no released API floor. Released clients
/// never accept unknown, malformed or older versions, including dev workers.
pub(crate) fn require_at_least(actual: &str, minimum: &str) -> Result<(), String> {
    if parse(actual)? < parse(minimum)? {
        return Err(format!("incompatible host service version {actual}: this client requires {minimum} or newer. Choose Retry or reconnect to update the host service."));
    }
    Ok(())
}

/// A download override can select a newer release, but cannot lower the API
/// requirements of the client that will send requests to it.
pub(crate) fn minimum(app: &str, configured: &str) -> Result<String, String> {
    Ok(std::cmp::max(parse(app)?, parse(configured)?).to_string())
}

/// Directory names can differ from the actual executable version when a pinned
/// download fell back to the latest release. Check the executable's own banner.
pub(crate) fn from_banner(output: &str) -> Result<String, String> {
    let parts: Vec<_> = output.split_whitespace().collect();
    if parts.len() != 3 || parts[0] != "skill-server" || parts[2] != "host-service=1" {
        return Err("The installed server does not report a supported durable host service version. Update its skill-server release and reconnect.".into());
    }
    parse(parts[1])?;
    Ok(parts[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_floor_uses_semver_precedence_without_downgrading_newer_hosts() {
        for (actual, required, accepted) in [
            ("1.2.4", "1.2.8", false),
            ("1.2.8", "1.2.8", true),
            ("1.2.10", "1.2.9", true),
            ("1.10.0", "1.9.0", true),
            ("1.2.9-rc.1", "1.2.9", false),
            ("1.2.9", "1.2.9-rc.1", true),
            ("1.2.9+build.1", "1.2.9+build.2", true),
            ("0.0.0", "1.2.8", false),
            ("1.2.8", "0.0.0", true),
            ("0.0.0", "0.0.0", true),
            ("", "1.2.8", false),
            ("unknown", "1.2.8", false),
            ("1.2", "1.2.8", false),
        ] {
            assert_eq!(require_at_least(actual, required).is_ok(), accepted, "{actual} vs {required}");
        }
        assert_eq!(minimum("1.2.9", "1.2.4").unwrap(), "1.2.9");
        assert_eq!(minimum("1.2.9", "1.3.0").unwrap(), "1.3.0");
        assert!(minimum("1.2.9", "unknown").is_err());
    }

    #[test]
    fn only_the_actual_executable_banner_establishes_a_version() {
        assert_eq!(from_banner("skill-server 1.2.10 host-service=1\n").unwrap(), "1.2.10");
        for output in [
            "skill-server 1.2.10",
            "skill-server old host-service=1",
            "skill-server 1.2.10 host-service=2",
            "unrelated 1.2.10 host-service=1",
            "skill-server 1.2.10 host-service=1\nextra output",
        ] {
            assert!(from_banner(output).is_err(), "{output}");
        }
    }
}
