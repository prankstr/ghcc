use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ureq::Agent;
use uuid::Uuid;

// GitHub's official Copilot client ID (same as VS Code extension)
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthFile {
    #[serde(rename = "github-copilot")]
    pub github_copilot: CopilotAuth,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CopilotAuth {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub refresh: String,
    pub access: String,
    pub expires: u64, // Unix timestamp in milliseconds
    #[serde(default)]
    pub api_endpoint: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub max_prompt_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    #[allow(dead_code)]
    token_type: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: u64,
    #[serde(default)]
    endpoints: HashMap<String, String>,
}

#[derive(Debug)]
pub enum AuthError {
    NotFound,
    Network(String),
    Parse(String),
    FileSystem(String),
    Auth(String),
    Timeout,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::NotFound => write!(f, "Credentials not found"),
            AuthError::Network(msg) => write!(f, "Network error: {}", msg),
            AuthError::Parse(msg) => write!(f, "Parse error: {}", msg),
            AuthError::FileSystem(msg) => write!(f, "File system error: {}", msg),
            AuthError::Auth(msg) => write!(f, "Authentication error: {}", msg),
            AuthError::Timeout => write!(f, "Authentication timed out"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Get the path to the auth file (~/.local/share/ghcc/auth.json)
pub fn auth_file_path() -> Result<PathBuf, AuthError> {
    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| AuthError::FileSystem("Could not determine data directory".into()))?;
    Ok(data_dir.join("ghcc").join("auth.json"))
}

/// Read existing auth from file
pub fn read_auth() -> Result<CopilotAuth, AuthError> {
    let path = auth_file_path()?;
    let content = fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AuthError::NotFound
        } else {
            AuthError::FileSystem(format!("Could not read {}: {}", path.display(), e))
        }
    })?;
    let auth_file: AuthFile = serde_json::from_str(&content)
        .map_err(|e| AuthError::Parse(format!("Invalid auth file: {}", e)))?;
    Ok(auth_file.github_copilot)
}

/// Save auth to file securely (atomic write with 0o600 permissions)
pub fn save_auth(auth: &CopilotAuth) -> Result<(), AuthError> {
    use std::io::Write;

    let path = auth_file_path()?;

    // Create parent directory if needed
    let parent = path
        .parent()
        .ok_or_else(|| AuthError::FileSystem("Invalid auth file path".into()))?;
    fs::create_dir_all(parent)
        .map_err(|e| AuthError::FileSystem(format!("Could not create directory: {}", e)))?;

    let auth_file = AuthFile {
        github_copilot: auth.clone(),
    };
    let content = serde_json::to_string_pretty(&auth_file)
        .map_err(|e| AuthError::Parse(format!("Could not serialize auth: {}", e)))?;

    // Write to temp file with secure permissions, then atomic rename
    let temp_path = parent.join(".auth.json.tmp");

    // Create file with 0o600 permissions from the start (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp_path)
            .map_err(|e| AuthError::FileSystem(format!("Could not create temp file: {}", e)))?;
        file.write_all(content.as_bytes())
            .map_err(|e| AuthError::FileSystem(format!("Could not write temp file: {}", e)))?;
    }

    // Atomic rename to final path
    fs::rename(&temp_path, &path)
        .map_err(|e| AuthError::FileSystem(format!("Could not save {}: {}", path.display(), e)))?;

    Ok(())
}

/// Check if the token is expired (with 5 minute buffer)
pub fn is_expired(auth: &CopilotAuth) -> bool {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Add 5 minute buffer
    let buffer_ms = 5 * 60 * 1000;
    auth.expires <= now_ms + buffer_ms
}

pub fn create_agent() -> Agent {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Perform the GitHub Device Flow login
pub fn login() -> Result<CopilotAuth, AuthError> {
    let agent = create_agent();

    // Step 1: Request device code
    eprintln!("Requesting device code...");
    let device_resp: DeviceCodeResponse = agent
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .send_form([("client_id", GITHUB_CLIENT_ID), ("scope", "read:user")])?
        .body_mut()
        .read_json()?;

    // Step 2: Show user the code and URL
    eprintln!();
    eprintln!("┌────────────────────────────────────────────────────┐");
    eprintln!("│  Please visit: {:<35} │", device_resp.verification_uri);
    eprintln!("│  And enter code: {:<33} │", device_resp.user_code);
    eprintln!("└────────────────────────────────────────────────────┘");
    eprintln!();
    eprint!("Waiting for authorization");
    io::stderr().flush().ok();

    // Step 3: Poll for access token
    let interval = Duration::from_secs(device_resp.interval.max(5));
    let deadline = Duration::from_secs(device_resp.expires_in);
    let start = std::time::Instant::now();

    let oauth_token = loop {
        if start.elapsed() > deadline {
            eprintln!();
            return Err(AuthError::Timeout);
        }

        thread::sleep(interval);
        eprint!(".");
        io::stderr().flush().ok();

        let resp: AccessTokenResponse = agent
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .send_form([
                ("client_id", GITHUB_CLIENT_ID),
                ("device_code", device_resp.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])?
            .body_mut()
            .read_json()?;

        if let Some(token) = resp.access_token {
            eprintln!(" done!");
            break token;
        }

        if let Some(error) = resp.error {
            match error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    thread::sleep(Duration::from_secs(5));
                    continue;
                }
                "expired_token" => {
                    eprintln!();
                    return Err(AuthError::Timeout);
                }
                "access_denied" => {
                    eprintln!();
                    return Err(AuthError::Auth(
                        resp.error_description
                            .unwrap_or_else(|| "Access denied".into()),
                    ));
                }
                _ => {
                    eprintln!();
                    return Err(AuthError::Auth(resp.error_description.unwrap_or(error)));
                }
            }
        }
    };

    // Step 4: Exchange OAuth token for Copilot token
    eprintln!("Fetching Copilot token...");
    let copilot_resp: CopilotTokenResponse = agent
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {}", oauth_token))
        .header("User-Agent", format!("ghcc/{}", env!("CARGO_PKG_VERSION")))
        .header(
            "Editor-Version",
            format!("ghcc/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()?
        .body_mut()
        .read_json()?;

    let auth = CopilotAuth {
        auth_type: "oauth".into(),
        refresh: oauth_token,
        access: copilot_resp.token,
        expires: copilot_resp.expires_at * 1000, // Convert to milliseconds
        api_endpoint: copilot_resp.endpoints.get("api").cloned(),
        model: None,
        machine_id: Some(Uuid::new_v4().to_string()),
        max_prompt_tokens: None, // Set when user selects a model
    };

    Ok(auth)
}

/// Refresh an expired access token using the refresh (OAuth) token
pub fn refresh_token(
    refresh: &str,
    current_model: Option<String>,
    current_machine_id: Option<String>,
    current_max_prompt_tokens: Option<u64>,
) -> Result<CopilotAuth, AuthError> {
    let agent = create_agent();

    let copilot_resp: CopilotTokenResponse = agent
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {}", refresh))
        .header("User-Agent", format!("ghcc/{}", env!("CARGO_PKG_VERSION")))
        .header(
            "Editor-Version",
            format!("ghcc/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()?
        .body_mut()
        .read_json()?;

    Ok(CopilotAuth {
        auth_type: "oauth".into(),
        refresh: refresh.to_string(),
        access: copilot_resp.token,
        expires: copilot_resp.expires_at * 1000,
        api_endpoint: copilot_resp.endpoints.get("api").cloned(),
        model: current_model,
        machine_id: current_machine_id,
        max_prompt_tokens: current_max_prompt_tokens,
    })
}

/// Get valid auth, refreshing if needed
pub fn get_valid_auth() -> Result<CopilotAuth, AuthError> {
    let mut auth = read_auth()?;

    if is_expired(&auth) || auth.api_endpoint.is_none() {
        // preserve current model, machine_id, and max_prompt_tokens during refresh
        let model = auth.model.clone();
        let machine_id = auth.machine_id.clone();
        let max_prompt_tokens = auth.max_prompt_tokens;
        auth = refresh_token(&auth.refresh, model, machine_id, max_prompt_tokens)?;
        save_auth(&auth)?;
    }

    Ok(auth)
}

// Implement From for ureq errors
impl From<ureq::Error> for AuthError {
    fn from(e: ureq::Error) -> Self {
        let msg = match &e {
            ureq::Error::StatusCode(401) => "Authentication failed. Try 'ghcc login'".to_string(),
            ureq::Error::StatusCode(403) => {
                "Access denied. Is your Copilot subscription active?".to_string()
            }
            ureq::Error::StatusCode(code) => format!("Request failed (HTTP {})", code),
            ureq::Error::Timeout(_) => "Request timed out. Check your connection.".to_string(),
            ureq::Error::HostNotFound => {
                "Could not reach GitHub. Check your internet connection.".to_string()
            }
            ureq::Error::ConnectionFailed => "Connection failed. Check your network.".to_string(),
            _ => format!("Network error: {}", e),
        };
        AuthError::Network(msg)
    }
}

impl From<std::io::Error> for AuthError {
    fn from(e: std::io::Error) -> Self {
        AuthError::FileSystem(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_auth_with_expires(expires_ms: u64) -> CopilotAuth {
        CopilotAuth {
            auth_type: "oauth".into(),
            refresh: "test_refresh".into(),
            access: "test_access".into(),
            expires: expires_ms,
            api_endpoint: None,
            model: None,
            machine_id: None,
            max_prompt_tokens: None,
        }
    }

    #[test]
    fn test_is_expired_returns_true_for_past_timestamp() {
        // Token expired 1 hour ago
        let one_hour_ago_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - (60 * 60 * 1000);

        let auth = make_auth_with_expires(one_hour_ago_ms);
        assert!(is_expired(&auth));
    }

    #[test]
    fn test_is_expired_returns_false_for_future_timestamp() {
        // Token expires in 1 hour
        let one_hour_from_now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + (60 * 60 * 1000);

        let auth = make_auth_with_expires(one_hour_from_now_ms);
        assert!(!is_expired(&auth));
    }

    #[test]
    fn test_is_expired_with_5_minute_buffer() {
        // Token expires in 4 minutes (within 5 minute buffer, so should be "expired")
        let four_mins_from_now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + (4 * 60 * 1000);

        let auth = make_auth_with_expires(four_mins_from_now_ms);
        assert!(
            is_expired(&auth),
            "Token expiring in 4 mins should be considered expired due to 5 min buffer"
        );

        // Token expires in 6 minutes (outside buffer, should NOT be expired)
        let six_mins_from_now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + (6 * 60 * 1000);

        let auth = make_auth_with_expires(six_mins_from_now_ms);
        assert!(
            !is_expired(&auth),
            "Token expiring in 6 mins should NOT be considered expired"
        );
    }

    #[test]
    fn test_auth_file_path_returns_expected_location() {
        let path = auth_file_path().expect("Should return a path");
        let path_str = path.to_string_lossy();

        assert!(
            path_str.contains("ghcc"),
            "Path should contain 'ghcc' directory"
        );
        assert!(
            path_str.ends_with("auth.json"),
            "Path should end with 'auth.json'"
        );
    }
}
