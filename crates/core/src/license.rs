//! Offline license key support (MVP).
//!
//! Model: license-key activation, no login (like RadioBoss profiles).
//! All features stay enabled while `Unlicensed` during development.
//!
//! Key format: `CB-XXXX-XXXX-XXXX`
//! - `X` = `A-Z` / `0-9` (normalized to uppercase, `-`/space ignored on input)
//! - Last 4 chars are a checksum of the first 8 payload chars + secret.
//!
//! Demo keys (for dev/tests):
//! - `CB-DEMO-DEMO-XXXX` where XXXX = valid checksum → 30-day trial
//! - `CB-PRO-0000-XXXX` → perpetual Pro (example)

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{CrabError, Result};

/// License tier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseTier {
    #[default]
    Trial,
    Standard,
    Pro,
}

/// Validated license info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseInfo {
    pub key: String,
    pub holder: String,
    pub tier: LicenseTier,
    /// `None` = perpetual.
    pub expires_at: Option<DateTime<Utc>>,
    pub activated_at: DateTime<Utc>,
}

impl LicenseInfo {
    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|exp| Utc::now() > exp)
    }

    pub fn status_label(&self) -> String {
        if self.is_expired() {
            return "License expired".to_string();
        }
        match self.expires_at {
            Some(exp) => format!(
                "Licensed ({:?}) — {} — expires {}",
                self.tier,
                self.holder,
                exp.format("%Y-%m-%d")
            ),
            None => format!("Licensed ({:?}) — {}", self.tier, self.holder),
        }
    }
}

/// Current license state (unlicensed still enables everything for now).
#[derive(Debug, Clone)]
pub enum LicenseStatus {
    Unlicensed,
    Licensed(LicenseInfo),
}

impl LicenseStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Unlicensed => "Unlicensed — all features enabled for now".to_string(),
            Self::Licensed(info) => info.status_label(),
        }
    }

    /// Gate features later; always true during MVP.
    pub fn features_enabled(&self) -> bool {
        match self {
            Self::Unlicensed => true,
            Self::Licensed(info) => !info.is_expired(),
        }
    }
}

// Secret for checksum (MVP — replace with real signature e.g. ed25519 later).
const CHECKSUM_SECRET: u32 = 0xC0FFEE;

/// Normalize user input: uppercase, keep only A-Z0-9.
fn normalize_key(input: &str) -> String {
    input
        .to_uppercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Checksum: FNV-ish over payload + secret, rendered as 4 base32-ish chars.
fn checksum_for(payload_8: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut hash: u32 = 0x811c9dc5 ^ CHECKSUM_SECRET;
    for b in payload_8.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    (0..4)
        .map(|i| {
            let idx = ((hash >> (i * 5)) & 31) as usize;
            ALPHABET[idx % ALPHABET.len()] as char
        })
        .collect()
}

/// Validate a key string into tier + expiry. Returns normalized key on success.
pub fn validate_key(input: &str) -> Result<(String, LicenseTier, Option<Duration>)> {
    let norm = normalize_key(input);
    if !norm.starts_with("CB") {
        return Err(CrabError::Library("License must start with CB-".into()));
    }
    let payload = norm.trim_start_matches("CB");
    if payload.len() != 12 {
        return Err(CrabError::Library(
            "License format: CB-XXXX-XXXX-XXXX".into(),
        ));
    }
    if !payload.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(CrabError::Library(
            "License contains invalid characters".into(),
        ));
    }
    let (data8, check4) = payload.split_at(8);
    let expected = checksum_for(data8);
    if check4 != expected {
        return Err(CrabError::Library("Invalid license checksum".into()));
    }

    // Tier + duration from payload prefix (MVP convention).
    let (tier, validity) = if data8.starts_with("DEMO") || data8.starts_with("TRIAL") {
        (LicenseTier::Trial, Some(Duration::days(30)))
    } else if data8.starts_with("STD") {
        (LicenseTier::Standard, None)
    } else if data8.starts_with("PRO") {
        (LicenseTier::Pro, None)
    } else {
        // Unknown prefix → treat as 30-day trial (keeps MVP open).
        (LicenseTier::Trial, Some(Duration::days(30)))
    };

    // Canonical display form: CB-XXXX-XXXX-XXXX
    let canonical = format!(
        "CB-{}-{}-{}",
        &payload[0..4],
        &payload[4..8],
        &payload[8..12]
    );
    Ok((canonical, tier, validity))
}

/// Build LicenseInfo from a validated key.
pub fn activate_key(input: &str, holder: &str) -> Result<LicenseInfo> {
    let (canonical, tier, validity) = validate_key(input)?;
    let now = Utc::now();
    Ok(LicenseInfo {
        key: canonical,
        holder: holder.to_string(),
        tier,
        expires_at: validity.map(|d| now + d),
        activated_at: now,
    })
}

/// Generate a valid demo/trial key (dev helper, not a bypass — same checksum path).
pub fn generate_demo_key() -> String {
    let data8 = "DEMODEMO";
    format!("CB-DEMO-DEMO-{}", checksum_for(data8))
}

/// File-backed license store (`license.json` next to the DB).
#[derive(Debug, Clone)]
pub struct LicenseStore {
    path: PathBuf,
    status: LicenseStatus,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredLicense {
    info: LicenseInfo,
}

impl LicenseStore {
    pub fn open(path: &Path) -> Self {
        let status = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<StoredLicense>(&s).ok())
            .map(|s| LicenseStatus::Licensed(s.info))
            .unwrap_or(LicenseStatus::Unlicensed);
        Self {
            path: path.to_path_buf(),
            status,
        }
    }

    pub fn status(&self) -> &LicenseStatus {
        &self.status
    }

    pub fn activate(&mut self, input: &str, holder: &str) -> Result<LicenseInfo> {
        let info = activate_key(input, holder)?;
        let stored = StoredLicense { info: info.clone() };
        let json =
            serde_json::to_string_pretty(&stored).map_err(|e| CrabError::Library(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, json)?;
        self.status = LicenseStatus::Licensed(info.clone());
        Ok(info)
    }

    pub fn clear(&mut self) -> Result<()> {
        let _ = std::fs::remove_file(&self.path);
        self.status = LicenseStatus::Unlicensed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_key_roundtrips() {
        let key = generate_demo_key();
        let (canonical, tier, _) = validate_key(&key).expect("demo key must validate");
        assert_eq!(canonical, key);
        assert_eq!(tier, LicenseTier::Trial);
    }

    #[test]
    fn rejects_bad_checksum() {
        assert!(validate_key("CB-AAAA-BBBB-CCCC").is_err());
    }

    #[test]
    fn rejects_bad_format() {
        assert!(validate_key("HELLO").is_err());
        assert!(validate_key("CB-SHORT").is_err());
    }
}
