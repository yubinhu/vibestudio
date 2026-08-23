//! Ship-time feature switches. Each const gates a finished feature at one
//! choke point, so turning it on for the next release is a one-line flip.

/// App auto-update. ON: the feed URL + signing pubkey in tauri.conf.json are now
/// the permanent update channel for the shipped fleet (yubinhu/vibestudio
/// releases). Builds register the installer and poll the release feed; only
/// builds shipped from here on self-update — earlier ones (≤ v0.1.1, shipped
/// while this was off) need a one-time manual upgrade.
pub const AUTO_UPDATE: bool = true;
