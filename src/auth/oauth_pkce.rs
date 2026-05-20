//! OAuth 2.0 PKCE authorization-code flow for Google and Discord.
//!
//! * No `client_secret` — PKCE replaces it.
//! * A short-lived localhost HTTP server receives the callback.
//! * The browser is opened via `opener`.
//! * Tokens are stored in the OS keychain (via `super::save_token`).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use color_eyre::eyre::{self, Context as _, bail};
use rand::Rng as _;
use reqwest::Client;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpListener as TokioTcpListener;

use crate::auth::{OAuthProvider, TokenSet, delete_token, save_token};

// ---------------------------------------------------------------------------
// Provider configuration
// ---------------------------------------------------------------------------

/// Google OAuth endpoints (from the well-known OpenID configuration).
const GOOGLE_AUTHORIZE_URL: &str =
    "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SCOPES: &str = "openid email profile";

/// Discord OAuth endpoints.
const DISCORD_AUTHORIZE_URL: &str = "https://discord.com/api/oauth2/authorize";
const DISCORD_TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const DISCORD_SCOPES: &str = "identify email";

/// Client IDs — these are public by design in PKCE.
/// In production, replace with your registered client IDs.
const GOOGLE_CLIENT_ID: &str = "YOUR_GOOGLE_CLIENT_ID.apps.googleusercontent.com";
const DISCORD_CLIENT_ID: &str = "YOUR_DISCORD_CLIENT_ID";

/// Redirect URI — must match what is registered in the provider console.
/// We use a loopback address; the port is chosen at runtime.
const REDIRECT_HOST: &str = "127.0.0.1";

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

/// Generate a cryptographically random `code_verifier` (43-128 chars,
/// URL-safe base64 without padding).
fn generate_code_verifier() -> String {
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute `code_challenge` = base64url(SHA256(code_verifier)).
fn generate_code_challenge(verifier: &str) -> String {
    use sha2::Digest as _;
    let hash = sha2::Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}

// ---------------------------------------------------------------------------
// Authorize URL builder
// ---------------------------------------------------------------------------

fn build_authorize_url(
    provider: OAuthProvider,
    client_id: &str,
    redirect_port: u16,
    code_challenge: &str,
    state: &str,
) -> String {
    let redirect_uri = format!("http://{REDIRECT_HOST}:{redirect_port}/callback");
    let (auth_url, scopes) = match provider {
        OAuthProvider::Google => (GOOGLE_AUTHORIZE_URL, GOOGLE_SCOPES),
        OAuthProvider::Discord => (DISCORD_AUTHORIZE_URL, DISCORD_SCOPES),
    };

    let mut params = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", &redirect_uri),
        ("scope", scopes),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];

    // Google requires `access_type=offline` to get a refresh_token.
    if matches!(provider, OAuthProvider::Google) {
        params.push(("access_type", "offline"));
        params.push(("prompt", "consent"));
    }

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{auth_url}?{query}")
}

// ---------------------------------------------------------------------------
// State token
// ---------------------------------------------------------------------------

fn generate_state() -> String {
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    /// Lifetime in seconds.
    expires_in: u64,
}

async fn exchange_code(
    provider: OAuthProvider,
    client_id: &str,
    redirect_port: u16,
    code: &str,
    code_verifier: &str,
) -> eyre::Result<TokenResponse> {
    let redirect_uri = format!("http://{REDIRECT_HOST}:{redirect_port}/callback");
    let token_url = match provider {
        OAuthProvider::Google => GOOGLE_TOKEN_URL,
        OAuthProvider::Discord => DISCORD_TOKEN_URL,
    };

    let mut params = HashMap::new();
    params.insert("grant_type", "authorization_code");
    params.insert("code", code);
    params.insert("redirect_uri", &redirect_uri);
    params.insert("code_verifier", code_verifier);

    // Discord requires client_id in the body (no client_secret for public clients).
    if matches!(provider, OAuthProvider::Discord) {
        params.insert("client_id", client_id);
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .wrap_err("failed to build HTTP client")?;

    let resp = client
        .post(token_url)
        .form(&params)
        .send()
        .await
        .wrap_err("token exchange request failed")?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .wrap_err("failed to read token response body")?;

    if !status.is_success() {
        bail!("token exchange failed ({status}): {body}");
    }

    let json: serde_json::Value =
        serde_json::from_str(&body).wrap_err("failed to parse token JSON")?;

    let access_token = json["access_token"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("missing access_token in response"))?
        .to_string();

    let refresh_token = json["refresh_token"].as_str().map(String::from);

    let expires_in = json["expires_in"]
        .as_u64()
        .unwrap_or(3600); // default 1 hour

    Ok(TokenResponse {
        access_token,
        refresh_token,
        expires_in,
    })
}

// ---------------------------------------------------------------------------
// Loopback server
// ---------------------------------------------------------------------------

/// Bind to port 0 (OS picks a free port) and return the listener + port.
fn bind_loopback() -> eyre::Result<(TcpListener, u16)> {
    let addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0);
    let listener = TcpListener::bind(addr).wrap_err("failed to bind loopback socket")?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// HTML page shown after successful login.
const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Login Successful</title>
<style>
body{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;
height:100vh;margin:0;background:#1a1a2e;color:#eee}
.card{background:#16213e;padding:2rem 3rem;border-radius:12px;text-align:center}
h1{color:#4ecca3;margin:0 0 .5rem}
p{color:#a8a8b3}
</style></head><body>
<div class="card"><h1>✓ Login Successful</h1>
<p>You may close this tab and return to the recorder.</p></div>
</body></html>"#;

/// HTML page shown on error.
const ERROR_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Login Failed</title>
<style>
body{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;
height:100vh;margin:0;background:#1a1a2e;color:#eee}
.card{background:#16213e;padding:2rem 3rem;border-radius:12px;text-align:center}
h1{color:#e94560;margin:0 0 .5rem}
p{color:#a8a8b3}
</style></head><body>
<div class="card"><h1>✗ Login Failed</h1>
<p>Something went wrong. Please try again.</p></div>
</body></html>"#;

/// Run a minimal HTTP server that waits for a single GET request on `/callback`.
///
/// Returns the query string (everything after `?`) or an error.
async fn wait_for_callback(listener: TcpListener) -> eyre::Result<String> {
    // Convert std TcpListener to tokio TcpListener
    listener.set_nonblocking(true)?;
    let tokio_listener = TokioTcpListener::from_std(listener)?;

    let (mut stream, _) = tokio_listener
        .accept()
        .await
        .wrap_err("failed to accept callback connection")?;

    let (reader, mut writer) = stream.split();
    let mut buf_reader = BufReader::new(reader);
    let mut first_line = String::new();
    buf_reader
        .read_line(&mut first_line)
        .await
        .wrap_err("failed to read HTTP request")?;

    // Parse: GET /callback?code=...&state=... HTTP/1.1
    let query = first_line
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split_once('?').map(|(_, q)| q.to_string()))
        .ok_or_else(|| eyre::eyre!("malformed HTTP request: {first_line}"))?;

    // Send response
    let html = if query.contains("error=") {
        ERROR_HTML
    } else {
        SUCCESS_HTML
    };

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    writer
        .write_all(response.as_bytes())
        .await
        .ok();
    writer.flush().await.ok();

    Ok(query)
}

// ---------------------------------------------------------------------------
// Parse callback query
// ---------------------------------------------------------------------------

fn parse_callback_query(query: &str) -> eyre::Result<(String, String)> {
    let mut code = None;
    let mut state = None;

    for pair in query.split('&') {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| eyre::eyre!("malformed query param: {pair}"))?;
        let decoded = urlencoding::decode(v)
            .wrap_err("failed to URL-decode query param")?;
        match k {
            "code" => code = Some(decoded.into_owned()),
            "state" => state = Some(decoded.into_owned()),
            _ => {}
        }
    }

    let code = code.ok_or_else(|| eyre::eyre!("missing 'code' in callback"))?;
    let state = state.ok_or_else(|| eyre::eyre!("missing 'state' in callback"))?;

    Ok((code, state))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the full PKCE authorization flow for the given provider.
///
/// 1. Bind a loopback listener (port 0).
/// 2. Generate PKCE challenge.
/// 3. Open the browser to the provider's authorize URL.
/// 4. Wait for the callback.
/// 5. Exchange the code for tokens.
/// 6. Persist tokens to the keychain.
///
/// This is a blocking function (spawns a tokio runtime internally if needed).
pub fn run_oauth_flow(provider: OAuthProvider) -> eyre::Result<TokenSet> {
    // Use an existing tokio runtime if available, otherwise create one.
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // We're inside a runtime — spawn a blocking task.
            let provider_clone = provider;
            tokio::task::block_in_place(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(run_oauth_flow_async(provider_clone))
            })
        }
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .wrap_err("failed to create tokio runtime")?;
            rt.block_on(run_oauth_flow_async(provider))
        }
    }
}

async fn run_oauth_flow_async(provider: OAuthProvider) -> eyre::Result<TokenSet> {
    let client_id = match provider {
        OAuthProvider::Google => GOOGLE_CLIENT_ID,
        OAuthProvider::Discord => DISCORD_CLIENT_ID,
    };

    // Step 1: bind loopback
    let (listener, port) = bind_loopback()?;
    tracing::info!("OAuth callback listener on port {port}");

    // Step 2: PKCE
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_state();

    // Step 3: build authorize URL and open browser
    let auth_url = build_authorize_url(provider, client_id, port, &code_challenge, &state);
    tracing::info!("Opening browser for OAuth: {provider}");
    tracing::debug!("Authorize URL: {auth_url}");

    // Store the expected state for validation
    let expected_state = Arc::new(state);

    // Open browser *before* waiting for callback (non-blocking)
    let open_result = opener::open(&auth_url);
    if let Err(e) = &open_result {
        tracing::warn!("Failed to auto-open browser: {e}. Please visit the URL manually.");
        // Don't fail — user can still paste the URL.
    }

    // Step 4: wait for callback
    tracing::info!("Waiting for OAuth callback...");
    let query = wait_for_callback(listener).await?;

    // Step 5: parse callback
    let (code, returned_state) = parse_callback_query(&query)?;
    if returned_state != *expected_state {
        bail!("state mismatch — possible CSRF attack");
    }

    // Step 6: exchange code for tokens
    tracing::info!("Exchanging authorization code for tokens...");
    let token_resp = exchange_code(provider, client_id, port, &code, &code_verifier).await?;

    // Step 7: build TokenSet
    let now = Utc::now();
    let token_set = TokenSet {
        provider,
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token,
        expires_at: now + chrono::Duration::seconds(token_resp.expires_in as i64),
        obtained_at: now,
    };

    // Step 8: persist
    save_token(&token_set).wrap_err("failed to save token to keychain")?;
    tracing::info!("OAuth login complete — token saved for {provider}");

    Ok(token_set)
}

/// Attempt to refresh an expired token.
///
/// Not all providers support refresh tokens (Discord does, Google does).
/// If no refresh_token is available, returns `None` and the caller should
/// re-run the full flow.
pub async fn try_refresh_token(tokens: &TokenSet) -> eyre::Result<Option<TokenSet>> {
    let refresh_token = match &tokens.refresh_token {
        Some(rt) => rt,
        None => return Ok(None),
    };

    let client_id = match tokens.provider {
        OAuthProvider::Google => GOOGLE_CLIENT_ID,
        OAuthProvider::Discord => DISCORD_CLIENT_ID,
    };

    let token_url = match tokens.provider {
        OAuthProvider::Google => GOOGLE_TOKEN_URL,
        OAuthProvider::Discord => DISCORD_TOKEN_URL,
    };

    let mut params = HashMap::new();
    params.insert("grant_type", "refresh_token");
    params.insert("refresh_token", refresh_token.as_str());

    if matches!(tokens.provider, OAuthProvider::Discord) {
        params.insert("client_id", client_id);
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let resp = client.post(token_url).form(&params).send().await?;
    let status = resp.status();
    let body = resp.text().await?;

    if !status.is_success() {
        tracing::warn!("Token refresh failed ({status}): {body}");
        return Ok(None);
    }

    let json: serde_json::Value = serde_json::from_str(&body)?;

    let access_token = json["access_token"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("missing access_token in refresh response"))?
        .to_string();

    let new_refresh_token = json["refresh_token"].as_str().map(String::from);
    let expires_in = json["expires_in"].as_u64().unwrap_or(3600);

    let now = Utc::now();
    let new_tokens = TokenSet {
        provider: tokens.provider,
        access_token,
        refresh_token: new_refresh_token.or_else(|| tokens.refresh_token.clone()),
        expires_at: now + chrono::Duration::seconds(expires_in as i64),
        obtained_at: now,
    };

    save_token(&new_tokens)?;
    tracing::info!("Token refreshed for {}", tokens.provider);

    Ok(Some(new_tokens))
}

/// Logout: delete stored tokens.
pub fn logout() -> eyre::Result<()> {
    delete_token()?;
    tracing::info!("Logged out — token deleted from keychain");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_verifier_length() {
        let v = generate_code_verifier();
        assert!(v.len() >= 43 && v.len() <= 128);
    }

    #[test]
    fn test_code_challenge_is_base64url() {
        let v = generate_code_verifier();
        let c = generate_code_challenge(&v);
        // SHA256 = 32 bytes → base64url no-pad = 43 chars
        assert_eq!(c.len(), 43);
        // Should only contain URL-safe base64 chars
        assert!(c
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'));
    }

    #[test]
    fn test_generate_state() {
        let s1 = generate_state();
        let s2 = generate_state();
        assert_ne!(s1, s2);
        assert_eq!(s1.len(), 22); // 16 bytes → base64url no-pad
    }

    #[test]
    fn test_build_authorize_url_google() {
        let url = build_authorize_url(
            OAuthProvider::Google,
            "test-client-id",
            54321,
            "test-challenge",
            "test-state",
        );
        assert!(url.starts_with(GOOGLE_AUTHORIZE_URL));
        assert!(url.contains("client_id=test-client-id"));
        assert!(url.contains("code_challenge=test-challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=test-state"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("scope=openid+email+profile"));
    }

    #[test]
    fn test_build_authorize_url_discord() {
        let url = build_authorize_url(
            OAuthProvider::Discord,
            "discord-client-id",
            54321,
            "test-challenge",
            "test-state",
        );
        assert!(url.starts_with(DISCORD_AUTHORIZE_URL));
        assert!(url.contains("client_id=discord-client-id"));
        assert!(url.contains("scope=identify+email"));
        assert!(!url.contains("access_type")); // Discord doesn't use this
    }

    #[test]
    fn test_parse_callback_query() {
        let query = "code=abc123&state=xyz789&scope=openid";
        let (code, state) = parse_callback_query(query).unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz789");
    }

    #[test]
    fn test_parse_callback_query_url_encoded() {
        let query = "code=abc%2Bdef&state=xyz%3D789";
        let (code, state) = parse_callback_query(query).unwrap();
        assert_eq!(code, "abc+def");
        assert_eq!(state, "xyz=789");
    }

    #[test]
    fn test_parse_callback_query_missing_code() {
        let query = "error=access_denied&state=xyz";
        let result = parse_callback_query(query);
        assert!(result.is_err());
    }

    #[test]
    fn test_token_set_validity() {
        let now = Utc::now();
        let valid_token = TokenSet {
            provider: OAuthProvider::Google,
            access_token: "tok".into(),
            refresh_token: None,
            expires_at: now + chrono::Duration::hours(1),
            obtained_at: now,
        };
        assert!(valid_token.is_valid());

        let expired_token = TokenSet {
            provider: OAuthProvider::Google,
            access_token: "tok".into(),
            refresh_token: None,
            expires_at: now - chrono::Duration::hours(1),
            obtained_at: now,
        };
        assert!(!expired_token.is_valid());
    }

    #[test]
    fn test_token_set_serialization_roundtrip() {
        let now = Utc::now();
        let tokens = TokenSet {
            provider: OAuthProvider::Discord,
            access_token: "discord-token".into(),
            refresh_token: Some("refresh-123".into()),
            expires_at: now + chrono::Duration::hours(1),
            obtained_at: now,
        };
        let json = serde_json::to_string(&tokens).unwrap();
        let decoded: TokenSet = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.provider, OAuthProvider::Discord);
        assert_eq!(decoded.access_token, "discord-token");
        assert_eq!(decoded.refresh_token, Some("refresh-123".into()));
    }

    #[cfg(feature = "mock-oauth")]
    mod mock_tests {
        use super::*;
        use std::net::TcpStream;
        use std::io::Write;

        /// Simulate a mock OAuth callback by connecting to the loopback server
        /// and sending a fake HTTP request.
        #[tokio::test]
        async fn test_mock_callback_receives_code() {
            let (listener, port) = bind_loopback().unwrap();
            tracing::info!("Mock test: listener on port {port}");

            // Spawn the callback waiter
            let waiter = tokio::spawn(async move { wait_for_callback(listener).await });

            // Give the server a moment to be ready
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Connect and send a mock callback
            let redirect_uri = format!("http://{REDIRECT_HOST}:{port}/callback?code=mock-code-123&state=mock-state");
            let request = format!("GET /callback?code=mock-code-123&state=mock-state HTTP/1.1\r\nHost: {REDIRECT_HOST}:{port}\r\nConnection: close\r\n\r\n");

            // Use std TcpStream for simplicity
            let mut stream = TcpStream::connect(format!("{REDIRECT_HOST}:{port}")).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            stream.flush().unwrap();

            // Read response (just to ensure the server processed it)
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).unwrap();

            let query = waiter.await.unwrap().unwrap();
            assert!(query.contains("code=mock-code-123"));
            assert!(query.contains("state=mock-state"));
        }

        #[tokio::test]
        async fn test_mock_callback_error_response() {
            let (listener, port) = bind_loopback().unwrap();
            let waiter = tokio::spawn(async move { wait_for_callback(listener).await });

            tokio::time::sleep(Duration::from_millis(50)).await;

            let request = format!(
                "GET /callback?error=access_denied&state=mock-state HTTP/1.1\r\nHost: {REDIRECT_HOST}:{port}\r\nConnection: close\r\n\r\n"
            );
            let mut stream = TcpStream::connect(format!("{REDIRECT_HOST}:{port}")).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            stream.flush().unwrap();

            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).unwrap();

            let query = waiter.await.unwrap().unwrap();
            assert!(query.contains("error=access_denied"));
        }
    }
}
