//! WinSparkle-style silent auto-update for the gamedata-recorder.
//!
//! ## Flow
//! 1. Every 24 h, fetch `appcast.xml` from the configured `update_server`.
//! 2. Verify the XML's ed25519 signature using the embedded public key.
//! 3. If the version in the feed is newer than `CARGO_PKG_VERSION`:
//!    a. Download the `setup.exe` to `%TEMP%`.
//!    b. Verify the downloaded binary's ed25519 signature.
//!    c. Quit the current process.
//!    d. Spawn `setup.exe /SILENT` (Inno Setup silent install).
//!
//! On any failure we log and retry on the next 24 h cycle — never block the
//! recorder.

use color_eyre::Result;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::MissedTickBehavior;
use tracing;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// How often to poll for updates (24 hours).
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Default update-server base URL. Can be overridden via env var
/// `GAMEDATA_UPDATE_SERVER_URL`.
const DEFAULT_UPDATE_SERVER_URL: &str = "https://updates.gamedata.example.com";

/// The ed25519 public key (hex-encoded) used to verify appcast signatures.
/// In production this is baked at build time; for dev/test it can be
/// overridden via `GAMEDATA_UPDATE_PUBKEY_HEX`.
const DEFAULT_PUBKEY_HEX: &str = "59e8e9c84f8e4e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e";

// ---------------------------------------------------------------------------
// Appcast types (RSS 2.0 + custom extensions)
// ---------------------------------------------------------------------------

/// Parsed appcast feed.
#[derive(Debug, Clone, Deserialize)]
pub struct Appcast {
    pub channel: Channel,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub title: String,
    pub link: String,
    #[serde(rename = "item", default)]
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub title: String,
    pub pub_date: Option<String>,
    #[serde(rename = "enclosure")]
    pub enclosure: Option<Enclosure>,
    #[serde(rename = "sparkle:version", default)]
    pub sparkle_version: Option<String>,
    #[serde(rename = "sparkle:shortVersionString", default)]
    pub sparkle_short_version: Option<String>,
    #[serde(rename = "sparkle:edSignature", default)]
    pub sparkle_ed_signature: Option<String>,
    #[serde(rename = "description", default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Enclosure {
    #[serde(rename = "url")]
    pub url: String,
    #[serde(rename = "length")]
    pub length: String,
    #[serde(rename = "type")]
    pub mime_type: String,
}

// ---------------------------------------------------------------------------
// Public key loading
// ---------------------------------------------------------------------------

/// Load the ed25519 verifying key from env or the compiled-in default.
fn load_verifying_key() -> Result<VerifyingKey> {
    let hex = std::env::var("GAMEDATA_UPDATE_PUBKEY_HEX")
        .unwrap_or_else(|_| DEFAULT_PUBKEY_HEX.to_string());

    let bytes = hex::decode(&hex)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to decode update public key hex: {e}"))?;

    let key_bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| color_eyre::eyre::eyre!("Public key must be 32 bytes"))?;

    VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| color_eyre::eyre::eyre!("Invalid ed25519 public key: {e}"))
}

// ---------------------------------------------------------------------------
// Fetch & verify appcast
// ---------------------------------------------------------------------------

/// Fetch the appcast XML from the update server and verify its signature.
///
/// The server returns two things:
/// - `appcast.xml` — the RSS feed
/// - `appcast.xml.sig` — the raw ed25519 signature (base64) of the XML bytes
///
/// We verify that `signature.verify(xml_bytes)` succeeds before trusting any
/// version information.
async fn fetch_verified_appcast(client: &reqwest::Client, base_url: &str) -> Result<Appcast> {
    let appcast_url = format!("{base_url}/appcast.xml");
    let sig_url = format!("{base_url}/appcast.xml.sig");

    // Fetch both in parallel
    let (appcast_resp, sig_resp) =
        tokio::try_join!(client.get(&appcast_url).send(), client.get(&sig_url).send())?;

    let appcast_bytes = appcast_resp
        .bytes()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Failed to read appcast body: {e}"))?;

    let sig_bytes = sig_resp
        .bytes()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Failed to read signature body: {e}"))?;

    // Decode the base64 signature
    let sig_text = String::from_utf8_lossy(&sig_bytes);
    let sig_bytes_decoded = base64_decode(sig_text.trim())?;
    let signature = Signature::from_slice(&sig_bytes_decoded)
        .map_err(|e| color_eyre::eyre::eyre!("Invalid signature format: {e}"))?;

    // Verify
    let vk = load_verifying_key()?;
    vk.verify(&appcast_bytes, &signature).map_err(|e| {
        color_eyre::eyre::eyre!(
            "ed25519 verification of appcast.xml FAILED (tampered or wrong key): {e}"
        )
    })?;

    tracing::info!("appcast.xml signature verified successfully");

    // Parse XML — we use quick-xml for a lightweight dependency
    let appcast: Appcast = quick_xml::de::from_slice(&appcast_bytes)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to parse appcast XML: {e}"))?;

    Ok(appcast)
}

// ---------------------------------------------------------------------------
// Download & verify setup.exe
// ---------------------------------------------------------------------------

/// Download the setup.exe to a temp path and verify its ed25519 signature.
///
/// The signature is taken from the `sparkle:edSignature` field in the appcast
/// item. The server also hosts `setup.exe.sig` (base64 ed25519 of the binary).
async fn download_and_verify_setup(client: &reqwest::Client, item: &Item) -> Result<PathBuf> {
    let enclosure = item
        .enclosure
        .as_ref()
        .ok_or_else(|| color_eyre::eyre::eyre!("No enclosure in appcast item"))?;

    let setup_url = &enclosure.url;

    // Download the binary
    tracing::info!(url = %setup_url, "Downloading setup.exe");
    let resp = client.get(setup_url).send().await?;
    let setup_bytes = resp.bytes().await?;

    // Verify the binary's signature from the appcast item
    let sig_hex = item
        .sparkle_ed_signature
        .as_ref()
        .ok_or_else(|| color_eyre::eyre::eyre!("No sparkle:edSignature in appcast item"))?;

    let sig_bytes = hex::decode(sig_hex)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to decode edSignature hex: {e}"))?;

    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|e| color_eyre::eyre::eyre!("Invalid edSignature format: {e}"))?;

    let vk = load_verifying_key()?;
    vk.verify(&setup_bytes, &signature).map_err(|e| {
        color_eyre::eyre::eyre!("ed25519 verification of setup.exe FAILED (tampered binary): {e}")
    })?;

    tracing::info!("setup.exe signature verified successfully");

    // Write to %TEMP%
    let temp_dir = std::env::temp_dir();
    let setup_path = temp_dir.join("gamedata-recorder-setup.exe");
    std::fs::write(&setup_path, &setup_bytes)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to write setup.exe to temp: {e}"))?;

    tracing::info!(path = ?setup_path, "Wrote setup.exe to temp");
    Ok(setup_path)
}

// ---------------------------------------------------------------------------
// Execute the update
// ---------------------------------------------------------------------------

/// Perform the silent update:
/// 1. Download & verify setup.exe
/// 2. Spawn `setup.exe /SILENT`
/// 3. Exit the current process
#[cfg(windows)]
async fn execute_update(setup_path: PathBuf) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    tracing::info!(path = ?setup_path, "Spawning setup.exe /SILENT");

    // CREATE_NEW_PROCESS_GROUP + DETACHED_PROCESS so the installer survives
    // after we exit.
    let _ = Command::new(&setup_path)
        .arg("/SILENT")
        .creation_flags(0x00000200 | 0x00000008) // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        .spawn();

    // Give the installer a moment to start
    tokio::time::sleep(Duration::from_secs(2)).await;

    tracing::info!("Exiting current process for update");
    std::process::exit(0);
}

#[cfg(not(windows))]
async fn execute_update(setup_path: PathBuf) -> Result<()> {
    // On non-Windows we just log — this is a Windows-only feature.
    tracing::warn!(
        path = ?setup_path,
        "Auto-update is Windows-only; skipping execution"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Main update check loop
// ---------------------------------------------------------------------------

/// Check for an update once. Returns `true` if an update was found and
/// initiated, `false` otherwise.
pub async fn check_for_update_once(client: &reqwest::Client) -> Result<bool> {
    let base_url = std::env::var("GAMEDATA_UPDATE_SERVER_URL")
        .unwrap_or_else(|_| DEFAULT_UPDATE_SERVER_URL.to_string());

    let current_version = env!("CARGO_PKG_VERSION");
    let current_semver = Version::parse(current_version)
        .map_err(|e| color_eyre::eyre::eyre!("Invalid CARGO_PKG_VERSION: {e}"))?;

    tracing::info!(
        current_version = %current_version,
        update_server = %base_url,
        "Checking for updates"
    );

    let appcast = match fetch_verified_appcast(client, &base_url).await {
        Ok(ac) => ac,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch/verify appcast; will retry next cycle");
            return Ok(false);
        }
    };

    // Find the latest item
    let latest = appcast
        .channel
        .items
        .iter()
        .filter_map(|item| {
            let ver_str = item
                .sparkle_version
                .as_ref()
                .or(item.sparkle_short_version.as_ref())?;
            let ver = Version::parse(ver_str.strip_prefix('v').unwrap_or(ver_str)).ok()?;
            Some((ver, item))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b));

    let Some((latest_ver, latest_item)) = latest else {
        tracing::info!("No valid version entries in appcast");
        return Ok(false);
    };

    if latest_ver <= current_semver {
        tracing::info!(
            current = %current_semver,
            latest = %latest_ver,
            "Already up to date"
        );
        return Ok(false);
    }

    tracing::info!(
        current = %current_semver,
        latest = %latest_ver,
        "New version available — downloading"
    );

    let setup_path = match download_and_verify_setup(client, latest_item).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to download/verify setup.exe; will retry next cycle");
            return Ok(false);
        }
    };

    // Execute the update (this exits the process on success)
    if let Err(e) = execute_update(setup_path).await {
        tracing::warn!(error = %e, "Failed to execute update; will retry next cycle");
        return Ok(false);
    }

    Ok(true)
}

/// Spawn a background task that checks for updates every 24 hours.
///
/// This is fire-and-forget — failures are logged but never propagated.
pub fn spawn_update_checker(client: reqwest::Client) {
    tokio::spawn(async move {
        // Stagger the first check by 5-15 minutes so we don't hammer the
        // server when many clients start simultaneously.
        let jitter = Duration::from_secs(300 + fastrand::u64(0..600));
        tracing::info!(
            ?jitter,
            "Update checker scheduled (first check after jitter)"
        );
        tokio::time::sleep(jitter).await;

        let mut interval = tokio::time::interval(UPDATE_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            match check_for_update_once(&client).await {
                Ok(true) => {
                    // Update was initiated — process is exiting, so this
                    // branch is unreachable in practice.
                    tracing::info!("Update initiated");
                    break;
                }
                Ok(false) => {
                    tracing::debug!("No update available");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Update check failed; will retry in 24h");
                }
            }
        }
    });
}

/// Compatibility hook for the half-wired S63v2 startup path.
///
/// The real updater needs runtime ownership and release/appcast configuration
/// before it can run safely. Keep startup buildable and no-op for now.
pub fn spawn_check_loop(_interval: Duration) {
    tracing::debug!("S63v2 updater startup hook disabled");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Minimal base64 decode (standard alphabet, no padding required).
fn base64_decode(input: &str) -> Result<Vec<u8>> {
    // Use the `base64` crate if available, otherwise a simple impl.
    // We'll add base64 as a dependency.
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, input)
        .map_err(|e| color_eyre::eyre::eyre!("base64 decode failed: {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    /// Generate a test keypair and return (signing_key, verifying_key_hex).
    fn test_keypair() -> (SigningKey, String) {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let vk_hex = hex::encode(signing_key.verifying_key().to_bytes());
        (signing_key, vk_hex)
    }

    #[test]
    fn test_version_comparison_newer() {
        let current = Version::parse("2.6.0").unwrap();
        let newer = Version::parse("2.7.0").unwrap();
        assert!(newer > current);
    }

    #[test]
    fn test_version_comparison_older() {
        let current = Version::parse("2.6.0").unwrap();
        let older = Version::parse("2.5.0").unwrap();
        assert!(older < current);
    }

    #[test]
    fn test_version_comparison_equal() {
        let current = Version::parse("2.6.0").unwrap();
        let same = Version::parse("2.6.0").unwrap();
        assert!(same == current);
    }

    #[test]
    fn test_version_comparison_patch() {
        let current = Version::parse("2.6.0").unwrap();
        let newer = Version::parse("2.6.1").unwrap();
        assert!(newer > current);
    }

    #[test]
    fn test_version_comparison_major() {
        let current = Version::parse("2.6.0").unwrap();
        let newer = Version::parse("3.0.0").unwrap();
        assert!(newer > current);
    }

    #[test]
    fn test_appcast_xml_parse() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>GameData Recorder Updates</title>
    <link>https://updates.gamedata.example.com</link>
    <item>
      <title>Version 2.7.0</title>
      <pubDate>Mon, 19 May 2026 00:00:00 +0000</pubDate>
      <enclosure
          url="https://updates.gamedata.example.com/gamedata-recorder-2.7.0-setup.exe"
          length="52428800"
          type="application/octet-stream"
          sparkle:edSignature="abcdef0123456789"
      />
      <sparkle:version>2.7.0</sparkle:version>
      <sparkle:shortVersionString>2.7.0</sparkle:shortVersionString>
      <description>Bug fixes and improvements.</description>
    </item>
  </channel>
</rss>"#;

        let appcast: Appcast = quick_xml::de::from_str(xml).expect("Failed to parse test appcast");
        assert_eq!(appcast.channel.title, "GameData Recorder Updates");
        assert_eq!(appcast.channel.items.len(), 1);
        let item = &appcast.channel.items[0];
        assert_eq!(item.sparkle_version.as_deref(), Some("2.7.0"));
        assert_eq!(
            item.enclosure.as_ref().unwrap().url,
            "https://updates.gamedata.example.com/gamedata-recorder-2.7.0-setup.exe"
        );
        assert_eq!(
            item.sparkle_ed_signature.as_deref(),
            Some("abcdef0123456789")
        );
    }

    #[test]
    fn test_appcast_xml_parse_multiple_items() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Updates</title>
    <link>https://example.com</link>
    <item>
      <title>Version 2.5.0</title>
      <sparkle:version>2.5.0</sparkle:version>
      <enclosure url="https://example.com/2.5.0.exe" length="1000" type="application/octet-stream"/>
    </item>
    <item>
      <title>Version 2.7.0</title>
      <sparkle:version>2.7.0</sparkle:version>
      <enclosure url="https://example.com/2.7.0.exe" length="2000" type="application/octet-stream"/>
    </item>
    <item>
      <title>Version 2.6.0</title>
      <sparkle:version>2.6.0</sparkle:version>
      <enclosure url="https://example.com/2.6.0.exe" length="1500" type="application/octet-stream"/>
    </item>
  </channel>
</rss>"#;

        let appcast: Appcast = quick_xml::de::from_str(xml).expect("Failed to parse");
        assert_eq!(appcast.channel.items.len(), 3);

        // Find max version
        let latest = appcast
            .channel
            .items
            .iter()
            .filter_map(|item| {
                let ver_str = item.sparkle_version.as_ref()?;
                Version::parse(ver_str.strip_prefix('v').unwrap_or(ver_str)).ok()
            })
            .max();

        assert_eq!(latest, Some(Version::parse("2.7.0").unwrap()));
    }

    #[test]
    fn test_ed25519_sign_and_verify_roundtrip() {
        let (signing_key, vk_hex) = test_keypair();

        // Override the pubkey for this test
        std::env::set_var("GAMEDATA_UPDATE_PUBKEY_HEX", &vk_hex);

        let message = b"test message for signing";
        let signature = signing_key.sign(message);

        let vk = load_verifying_key().expect("Failed to load verifying key");
        vk.verify(message, &signature).expect("Verification failed");
    }

    #[test]
    fn test_ed25519_verify_tampered_message_fails() {
        let (signing_key, vk_hex) = test_keypair();
        std::env::set_var("GAMEDATA_UPDATE_PUBKEY_HEX", &vk_hex);

        let message = b"original message";
        let tampered = b"tampered message";
        let signature = signing_key.sign(message);

        let vk = load_verifying_key().expect("Failed to load verifying key");
        let result = vk.verify(tampered, &signature);
        assert!(
            result.is_err(),
            "Verification should fail for tampered message"
        );
    }

    #[test]
    fn test_ed25519_verify_wrong_key_fails() {
        let (signing_key_a, vk_hex_a) = test_keypair();
        let (_signing_key_b, vk_hex_b) = test_keypair();

        // Set pubkey to key B
        std::env::set_var("GAMEDATA_UPDATE_PUBKEY_HEX", &vk_hex_b);

        let message = b"message signed by A";
        let signature = signing_key_a.sign(message);

        // Try to verify with key B's public key
        let vk = load_verifying_key().expect("Failed to load verifying key");
        let result = vk.verify(message, &signature);
        assert!(result.is_err(), "Verification should fail with wrong key");
    }

    #[test]
    fn test_base64_decode_roundtrip() {
        let original = b"hello world";
        let encoded = base64::engine::general_purpose::STANDARD.encode(original);
        let decoded = base64_decode(&encoded).expect("Decode failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_base64_decode_invalid_fails() {
        let result = base64_decode("!!!not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_verifying_key_invalid_hex() {
        std::env::set_var("GAMEDATA_UPDATE_PUBKEY_HEX", "zzzz");
        let result = load_verifying_key();
        assert!(result.is_err());
    }

    #[test]
    fn test_load_verifying_key_wrong_length() {
        std::env::set_var("GAMEDATA_UPDATE_PUBKEY_HEX", "abcd");
        let result = load_verifying_key();
        assert!(result.is_err());
    }

    /// Integration-style test: sign a fake appcast, verify it, parse it,
    /// and confirm the version is detected as newer.
    #[test]
    fn test_full_appcast_sign_verify_parse() {
        let (signing_key, vk_hex) = test_keypair();
        std::env::set_var("GAMEDATA_UPDATE_PUBKEY_HEX", &vk_hex);

        let appcast_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Test Updates</title>
    <link>https://test.example.com</link>
    <item>
      <title>Version 99.0.0</title>
      <sparkle:version>99.0.0</sparkle:version>
      <sparkle:shortVersionString>99.0.0</sparkle:shortVersionString>
      <sparkle:edSignature>deadbeef</sparkle:edSignature>
      <enclosure url="https://test.example.com/setup.exe" length="1000" type="application/octet-stream"/>
    </item>
  </channel>
</rss>"#;

        // Sign the XML
        let signature = signing_key.sign(appcast_xml.as_bytes());
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

        // Verify
        let vk = load_verifying_key().expect("Failed to load key");
        let sig = Signature::from_slice(&signature.to_bytes()).expect("Invalid sig");
        vk.verify(appcast_xml.as_bytes(), &sig)
            .expect("Verify failed");

        // Parse
        let appcast: Appcast = quick_xml::de::from_str(appcast_xml).expect("Parse failed");
        assert_eq!(appcast.channel.items.len(), 1);

        let item = &appcast.channel.items[0];
        let ver_str = item.sparkle_version.as_ref().unwrap();
        let ver = Version::parse(ver_str).unwrap();
        let current = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        assert!(ver > current, "99.0.0 should be newer than current version");

        // Verify the sig_b64 is valid base64
        let decoded = base64_decode(&sig_b64).expect("sig_b64 decode failed");
        assert_eq!(decoded.len(), 64); // ed25519 signature is 64 bytes
    }
}
