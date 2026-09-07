// Ported from Herdr, Copyright Herdr contributors, Apache-2.0.
// Upstream: 4b5e9bda239a0b6903889062d756424578e94691; see server/skill-core/src/agent_detection/NOTICE.txt.

use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, fmt};

#[derive(Debug, Clone)]
pub(crate) struct ManifestVersion(String);

impl ManifestVersion {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("version must not be empty".to_string());
        }
        for segment in trimmed.split('.') {
            if segment.is_empty() {
                return Err(format!("version {trimmed:?} contains an empty segment"));
            }
            if !segment.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(format!("version {trimmed:?} must be dotted numeric"));
            }
            segment
                .parse::<u64>()
                .map_err(|_| format!("version {trimmed:?} contains an oversized segment"))?;
        }
        Ok(Self(trimmed.to_string()))
    }
}

impl fmt::Display for ManifestVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ManifestVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ManifestVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl Ord for ManifestVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let mut left = self.0.split('.');
        let mut right = other.0.split('.');

        loop {
            match (left.next(), right.next()) {
                (Some(left), Some(right)) => {
                    let left = left.parse::<u64>().unwrap_or(0);
                    let right = right.parse::<u64>().unwrap_or(0);
                    match left.cmp(&right) {
                        Ordering::Equal => {}
                        ordering => return ordering,
                    }
                }
                (Some(left), None) => {
                    let left = left.parse::<u64>().unwrap_or(0);
                    if left == 0 {
                        continue;
                    }
                    return Ordering::Greater;
                }
                (None, Some(right)) => {
                    let right = right.parse::<u64>().unwrap_or(0);
                    if right == 0 {
                        continue;
                    }
                    return Ordering::Less;
                }
                (None, None) => return Ordering::Equal,
            }
        }
    }
}

impl PartialOrd for ManifestVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for ManifestVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ManifestVersion {}
