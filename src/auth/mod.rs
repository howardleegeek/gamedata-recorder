//! Authentication module — token storage via OS keychain (keyring crate).
//!
//! Tokens are stored encrypted in the platform keychain under the service
//! name `"oyster"` and the user `"auth"`.  The stored payload is a JSON
//! blob containing `access_token`, `refresh_token`, `expires_at`, and
//! `provider`.

mod oauth_pkce;

pub use oauth_pkce::*;

use chrono::{DateTime, Utc};
use color_eyre::eyre::{self, Context as _};
use keyring::Entry;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const KEYRING_SERVICE: &str = "oyster";
const KEYRING_USER: &str = "auth";

// ---------------------------------------------------------------------------
// Token model
// ---------------------------------------------------------------------------

/// Supported OAuth providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Google,
    Discord,
}

impl std::fmt::Display for OAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthProvider::Google => write!(f, "google"),
            OAuthProvider::Discord => write!(f, "discord"),
        }
    }
}

/// Persisted token payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub provider: OAuthProvider,
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Absolute UTC time when the access token expires.
    pub expires_at: DateTime<Utc>,
    /// When the token was last refreshed / obtained.
    pub obtained_at: DateTime<Utc>,
}

impl TokenSet {
    /// Returns `true` if the access token is still valid (with a 60-second
    /// grace period to avoid edge-case race conditions).
    pub fn is_valid(&self) -> bool {
        Utc::now() + chrono::Duration::seconds(60) < self.expires_at
    }
}

// ---------------------------------------------------------------------------
// Keyring helpers
// ---------------------------------------------------------------------------

fn keyring_entry() -> Result<Entry, keyring::Error> {
    Entry::new(KEYRING_SERVICE, KEYRING_USER)
}

/// Load the stored `TokenSet` from the OS keychain.
///
/// Returns `Ok(None)` when no token has been stored yet.
pub fn load_token() -> eyre::Result<Option<TokenSet>> {
    let entry = keyring_entry().wrap_err("failed to create keyring entry")?;
    match entry.get_password() {
        Ok(json) => {
            let tokens: TokenSet =
                serde_json::from_str(&json).wrap_err("failed to deserialize stored token")?;
            Ok(Some(tokens))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(eyre::eyre!("keyring read error: {e}")),
    }
}

/// Persist a `TokenSet` into the OS keychain.
pub fn save_token(tokens: &TokenSet) -> eyre::Result<()> {
    let entry = keyring_entry().wrap_err("failed to create keyring entry")?;
    let json = serde_json::to_string(tokens).wrap_err("failed to serialize token")?;
    entry
        .set_password(&json)
        .wrap_err("failed to write token to keyring")?;
    Ok(())
}

/// Delete the stored token from the OS keychain (used by Logout).
pub fn delete_token() -> eyre::Result<()> {
    let entry = keyring_entry().wrap_err("failed to create keyring entry")?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()), // already gone
        Err(e) => Err(eyre::eyre!("keyring delete error: {e}")),
    }
}
