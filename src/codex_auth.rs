use crate::config::{AppConfig, CodexAuth};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMode {
    Browser,
    Device,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(alias = "user_code", alias = "usercode")]
    user_code: String,
    #[serde(default)]
    interval: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

fn random_url_token(bytes: usize) -> String {
    let mut value = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn pkce_pair() -> (String, String) {
    let verifier = random_url_token(64);
    let digest = Sha256::digest(verifier.as_bytes());
    (verifier, URL_SAFE_NO_PAD.encode(digest))
}

fn parse_interval(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .unwrap_or(5)
        .max(1)
}

fn account_id_from_jwt(token: &str) -> String {
    let Some(payload) = token.split('.').nth(1) else {
        return String::new();
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return String::new();
    };
    value
        .pointer("/https://api.openai.com~1auth/chatgpt_account_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            value
                .pointer("/https://api.openai.com~1auth/account_id")
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            value
                .get("chatgpt_account_id")
                .and_then(|value| value.as_str())
        })
        .unwrap_or_default()
        .to_string()
}

fn save_tokens(tokens: TokenResponse) -> Result<CodexAuth, String> {
    let account_id = tokens
        .id_token
        .as_deref()
        .map(account_id_from_jwt)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| account_id_from_jwt(&tokens.access_token));
    let auth = CodexAuth {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token.unwrap_or_default(),
        account_id,
    };
    AppConfig::set_codex_auth(&auth)?;
    Ok(auth)
}

async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenResponse, String> {
    let response = client
        .post(format!("{ISSUER}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|error| format!("OAuth token exchange failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "OAuth token exchange failed with {status}: {}",
            response.text().await.unwrap_or_default()
        ));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Invalid OAuth token response: {error}"))
}

pub async fn login(mode: LoginMode) -> Result<CodexAuth, String> {
    match mode {
        LoginMode::Device => device_login().await,
        LoginMode::Browser => browser_login().await,
    }
}

async fn device_login() -> Result<CodexAuth, String> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{ISSUER}/api/accounts/deviceauth/usercode"))
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .map_err(|error| format!("Device login request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Device login request failed with {status}: {}",
            response.text().await.unwrap_or_default()
        ));
    }
    let code: DeviceCodeResponse = response
        .json()
        .await
        .map_err(|error| format!("Invalid device login response: {error}"))?;
    println!(
        "\nOpen https://auth.openai.com/codex/device and enter this code:\n\n  {}\n",
        code.user_code
    );

    let interval = parse_interval(&code.interval);
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let token = loop {
        if Instant::now() >= deadline {
            return Err("Device login timed out after 15 minutes".to_string());
        }
        let response = client
            .post(format!("{ISSUER}/api/accounts/deviceauth/token"))
            .json(&serde_json::json!({
                "device_auth_id": code.device_auth_id,
                "user_code": code.user_code,
            }))
            .send()
            .await
            .map_err(|error| format!("Device login polling failed: {error}"))?;
        if response.status().is_success() {
            break response
                .json::<DeviceTokenResponse>()
                .await
                .map_err(|error| format!("Invalid device token response: {error}"))?;
        }
        if response.status().as_u16() != 403 && response.status().as_u16() != 404 {
            return Err(format!("Device login failed with {}", response.status()));
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    };

    let tokens = exchange_code(
        &client,
        &token.authorization_code,
        &format!("{ISSUER}/deviceauth/callback"),
        &token.code_verifier,
    )
    .await?;
    save_tokens(tokens)
}

async fn browser_login() -> Result<CodexAuth, String> {
    let listener = TcpListener::bind("127.0.0.1:1455")
        .await
        .map_err(|error| format!("Could not start local login callback: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("Could not read login callback address: {error}"))?
        .port();
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let state = random_url_token(32);
    let (verifier, challenge) = pkce_pair();
    let mut url = Url::parse(&format!("{ISSUER}/oauth/authorize"))
        .map_err(|error| format!("Could not build login URL: {error}"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair(
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        )
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", &state)
        .append_pair("originator", "codex_cli_rs");
    let url = url.to_string();
    println!("Open this URL to sign in:\n\n{url}\n");
    open_browser(&url);

    let (mut socket, _) = listener
        .accept()
        .await
        .map_err(|error| format!("Login callback failed: {error}"))?;
    let mut buffer = vec![0u8; 16 * 1024];
    let length = socket
        .read(&mut buffer)
        .await
        .map_err(|error| format!("Could not read login callback: {error}"))?;
    let request = String::from_utf8_lossy(&buffer[..length]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| "Invalid login callback".to_string())?;
    let callback = Url::parse(&format!("http://localhost{target}"))
        .map_err(|error| format!("Invalid login callback URL: {error}"))?;
    let callback_state = callback
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    if callback_state != state {
        return Err("Login callback state mismatch".to_string());
    }
    let code = callback
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| "Login callback did not include an authorization code".to_string())?;
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<h2>Holiday login complete</h2><p>You can close this window.</p>")
        .await
        .map_err(|error| format!("Could not complete login callback: {error}"))?;

    let tokens = exchange_code(&reqwest::Client::new(), &code, &redirect_uri, &verifier).await?;
    save_tokens(tokens)
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        // Use Windows' URL protocol handler directly so query separators
        // (`&`) are passed to the default browser instead of cmd.exe.
        let _ = std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

pub fn parse_login_mode(value: Option<&str>) -> Result<LoginMode, String> {
    match value.unwrap_or("browser").to_ascii_lowercase().as_str() {
        "browser" | "web" => Ok(LoginMode::Browser),
        "device" | "device-code" => Ok(LoginMode::Device),
        value => Err(format!(
            "Unknown login mode '{value}'. Use browser or device."
        )),
    }
}

pub async fn refresh(auth: &CodexAuth) -> Result<CodexAuth, String> {
    if auth.refresh_token.trim().is_empty() {
        return Err("No Codex refresh token is stored".to_string());
    }
    let response = reqwest::Client::new()
        .post(format!("{ISSUER}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", auth.refresh_token.as_str()),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|error| format!("Token refresh failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Token refresh failed with {}", response.status()));
    }
    let mut tokens: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("Invalid refresh response: {error}"))?;
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(auth.refresh_token.clone());
    }
    let refreshed = save_tokens(tokens)?;
    if refreshed.account_id.is_empty() && !auth.account_id.is_empty() {
        let refreshed = CodexAuth {
            account_id: auth.account_id.clone(),
            ..refreshed
        };
        AppConfig::set_codex_auth(&refreshed)?;
        return Ok(refreshed);
    }
    Ok(refreshed)
}

pub fn login_error(error: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, error.into())
}
