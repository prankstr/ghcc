use crate::auth::{self, CopilotAuth};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use uuid::Uuid;

pub const DEFAULT_MODEL: &str = "gpt-5-mini";
pub const DEFAULT_MAX_PROMPT_TOKENS: u64 = 128_000;

// Rough estimate: 1 token ≈ 4 bytes for code
const BYTES_PER_TOKEN: usize = 4;

// Reserve tokens for system prompt, user prompt wrapper, and response
const RESERVED_TOKENS: u64 = 4_000;

#[derive(Debug, Clone, Copy, Default)]
pub enum CommitStyle {
    #[default]
    SingleLine,
    Detailed,
    Auto,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    messages: Vec<ChatMessage>,
    model: String,
    temperature: f32,
    top_p: f32,
    n: i32,
    stream: bool,
    intent: bool,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<Model>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub vendor: String,
    #[serde(default)]
    pub model_picker_enabled: bool,
    #[serde(default)]
    pub capabilities: Option<ModelCapabilities>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub limits: Option<ModelLimits>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ModelLimits {
    #[serde(default)]
    pub max_prompt_tokens: Option<u64>,
}

#[derive(Debug)]
pub enum CopilotError {
    Auth(auth::AuthError),
    Network(String),
    Api(String),
}

impl std::fmt::Display for CopilotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopilotError::Auth(e) => write!(f, "Auth error: {}", e),
            CopilotError::Network(msg) => write!(f, "Network error: {}", msg),
            CopilotError::Api(msg) => write!(f, "API error: {}", msg),
        }
    }
}

impl std::error::Error for CopilotError {}

impl From<auth::AuthError> for CopilotError {
    fn from(e: auth::AuthError) -> Self {
        CopilotError::Auth(e)
    }
}

impl From<ureq::Error> for CopilotError {
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
        CopilotError::Network(msg)
    }
}

impl From<std::io::Error> for CopilotError {
    fn from(e: std::io::Error) -> Self {
        CopilotError::Network(e.to_string())
    }
}

pub fn list_models(auth: &CopilotAuth) -> Result<Vec<Model>, CopilotError> {
    let endpoint = auth
        .api_endpoint
        .as_deref()
        .ok_or_else(|| CopilotError::Api("No API endpoint found in auth".into()))?;

    let url = format!("{}/models", endpoint);
    let agent = auth::create_agent();

    let response: ModelsResponse = agent
        .get(&url)
        .header("Authorization", format!("Bearer {}", auth.access))
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("Editor-Version", "vscode/1.95.0")
        .call()?
        .body_mut()
        .read_json()?;

    // Filter only enabled models
    let enabled_models = response
        .data
        .into_iter()
        .filter(|m| m.model_picker_enabled)
        .collect();

    Ok(enabled_models)
}

/// Get max diff bytes based on model's token limit
pub(crate) fn get_max_diff_bytes(max_prompt_tokens: Option<u64>) -> usize {
    let max_tokens = max_prompt_tokens.unwrap_or(DEFAULT_MAX_PROMPT_TOKENS);
    let available_tokens = max_tokens.saturating_sub(RESERVED_TOKENS);
    (available_tokens as usize) * BYTES_PER_TOKEN
}

/// Strip markdown code blocks from AI response
fn strip_markdown_code_block(text: &str) -> String {
    let text = text.trim();

    // Check if wrapped in code block (``` or ```text, ```commit, etc.)
    if text.starts_with("```") && text.ends_with("```") {
        // Find the end of the opening line (handles ```text, ```commit, etc.)
        let start = text.find('\n').map(|i| i + 1).unwrap_or(3);
        // Remove the closing ```
        let end = text.len() - 3;
        if start < end {
            return text[start..end].trim().to_string();
        }
    }

    text.to_string()
}

fn lowercase_scope(message: &str) -> String {
    let first_newline_index = message.find('\n').unwrap_or(message.len());
    let first_line = &message[..first_newline_index];

    let Some(open_index) = first_line.find('(') else {
        return message.to_string();
    };

    let Some(close_relative_index) = first_line[open_index + 1..].find(')') else {
        return message.to_string();
    };
    let close_index = open_index + 1 + close_relative_index;

    let scope = &first_line[open_index + 1..close_index];
    let lowercased_scope = scope.to_lowercase();
    if lowercased_scope == scope {
        return message.to_string();
    }

    let rest = &message[first_newline_index..];

    format!(
        "{}{}{}{}",
        &first_line[..open_index + 1],
        lowercased_scope,
        &first_line[close_index..],
        rest
    )
}

/// Base prompt template shared across all commit styles
const BASE_PROMPT: &str = "Generate a conventional commit message for this diff.

Rules:
- Format: type(scope): description
- Scope: noun for affected area. Omit if broad.
- Breaking changes: append ! to type.
- Keep subject under 72 characters.
- Describe intent, not implementation.
- Output ONLY the commit message.";

/// Build the prompt for generating a commit message
pub(crate) fn build_prompt(
    diff: &str,
    diff_stat: &str,
    style: CommitStyle,
    is_truncated: bool,
) -> String {
    let truncation_notice = if is_truncated {
        "\nNote: This diff was truncated due to size. Focus on the visible changes.\n"
    } else {
        ""
    };

    let style_instruction = match style {
        CommitStyle::SingleLine => "Output ONLY a single-line subject.",
        CommitStyle::Detailed => {
            "Output format:
- Subject line (under 72 chars)
- Blank line
- Bullet list of key changes, grouped by feature (- prefix)"
        }
        CommitStyle::Auto => {
            "Include a body (blank line + bullet list) ONLY if:
- The change is too complex to capture in the subject alone
- Breaking changes need explanation

Otherwise output ONLY the subject line."
        }
    };

    format!(
        "{BASE_PROMPT}

{style_instruction}

Files changed:
{diff_stat}
{truncation_notice}
Diff:
{diff}"
    )
}

pub fn generate_commit_message(
    auth: &CopilotAuth,
    diff: &str,
    diff_stat: &str,
    style: CommitStyle,
) -> Result<String, CopilotError> {
    let endpoint = auth
        .api_endpoint
        .as_deref()
        .ok_or_else(|| CopilotError::Api("No API endpoint found in auth".into()))?;

    let url = format!("{}/chat/completions", endpoint);
    let agent = auth::create_agent();

    let model = auth.model.as_deref().unwrap_or(DEFAULT_MODEL);

    // Get max diff size based on model's context limit
    let max_diff_bytes = get_max_diff_bytes(auth.max_prompt_tokens);
    if max_diff_bytes == 0 {
        return Err(CopilotError::Api(
            "Model context limit too small for commit generation".into(),
        ));
    }

    // Truncate diff if too large (at a valid UTF-8 boundary)
    let is_truncated = diff.len() > max_diff_bytes;
    let diff = if is_truncated {
        eprintln!(
            "Warning: Diff is large ({} bytes), truncating to ~{} bytes",
            diff.len(),
            max_diff_bytes
        );
        // Find the last valid UTF-8 char boundary at or before max_diff_bytes
        let mut end = max_diff_bytes;
        while end > 0 && !diff.is_char_boundary(end) {
            end -= 1;
        }
        &diff[..end]
    } else {
        diff
    };

    let prompt = build_prompt(diff, diff_stat, style, is_truncated);

    let request = ChatRequest {
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are an expert software engineer. Output only the raw commit message. No explanations, markdown, or extra text."
                    .to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: prompt,
            },
        ],
        model: model.to_string(),
        temperature: 0.0,
        top_p: 1.0,
        n: 1,
        stream: true,
        intent: true,
    };

    let session_id = Uuid::new_v4().to_string();
    let machine_id = auth
        .machine_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let response = agent
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth.access))
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("Editor-Version", "vscode/1.95.0")
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("x-request-id", &session_id)
        .header("vscode-sessionid", &session_id)
        .header("vscode-machineid", &machine_id)
        .send_json(&request)?;

    let mut reader = BufReader::new(response.into_body().into_reader());
    let mut line = String::new();
    let mut full_message = String::new();
    let mut chunks_received = 0;

    while reader.read_line(&mut line)? > 0 {
        if let Some(data) = line.strip_prefix("data: ") {
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }

            if let Ok(stream_resp) = serde_json::from_str::<StreamResponse>(data) {
                if let Some(content) = stream_resp
                    .choices
                    .first()
                    .and_then(|c| c.delta.content.as_ref())
                {
                    chunks_received += 1;
                    full_message.push_str(content);
                }
            }
        }
        line.clear();
    }

    if chunks_received == 0 {
        return Err(CopilotError::Api(
            "No valid response chunks from Copilot".into(),
        ));
    }

    let message = full_message.trim();

    // Strip markdown code blocks if present (AI sometimes wraps output in ```)
    let message = strip_markdown_code_block(message);
    let message = lowercase_scope(&message);

    if message.is_empty() {
        return Err(CopilotError::Api("Empty response from Copilot".into()));
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_max_diff_bytes_uses_default_when_none() {
        let bytes = get_max_diff_bytes(None);
        // Default: 128_000 tokens - 4_000 reserved = 124_000 tokens * 4 bytes = 496_000 bytes
        assert_eq!(bytes, 496_000);
    }

    #[test]
    fn test_get_max_diff_bytes_respects_custom_limit() {
        let bytes = get_max_diff_bytes(Some(10_000));
        // 10_000 tokens - 4_000 reserved = 6_000 tokens * 4 bytes = 24_000 bytes
        assert_eq!(bytes, 24_000);
    }

    #[test]
    fn test_get_max_diff_bytes_handles_small_limit() {
        // If limit is smaller than reserved, should saturate to 0
        let bytes = get_max_diff_bytes(Some(1_000));
        // 1_000 - 4_000 saturates to 0
        assert_eq!(bytes, 0);
    }

    #[test]
    fn test_prompt_single_line_format() {
        let diff = "diff --git a/file.rs";
        let diff_stat = "file.rs | 10 ++++++++++";
        let prompt = build_prompt(diff, diff_stat, CommitStyle::SingleLine, false);

        assert!(prompt.contains("single-line"));
        assert!(prompt.contains("72 characters"));
        assert!(prompt.contains("type(scope): description"));
        assert!(prompt.contains(diff));
        assert!(prompt.contains(diff_stat));
    }

    #[test]
    fn test_prompt_detailed_format() {
        let diff = "diff --git a/file.rs";
        let diff_stat = "file.rs | 10 ++++++++++";
        let prompt = build_prompt(diff, diff_stat, CommitStyle::Detailed, false);

        assert!(prompt.contains("Bullet list"));
        assert!(prompt.contains("Blank line"));
        assert!(prompt.contains("- prefix"));
        assert!(prompt.contains(diff));
        assert!(prompt.contains(diff_stat));
    }

    #[test]
    fn test_prompt_auto_format() {
        let diff = "diff --git a/file.rs";
        let diff_stat = "file.rs | 10 ++++++++++";
        let prompt = build_prompt(diff, diff_stat, CommitStyle::Auto, false);

        assert!(prompt.contains("too complex to capture"));
        assert!(prompt.contains("Breaking changes"));
        assert!(prompt.contains("ONLY the subject line"));
        assert!(prompt.contains(diff));
        assert!(prompt.contains(diff_stat));
    }

    #[test]
    fn test_prompt_contains_core_rules() {
        let diff = "test";
        let diff_stat = "file.rs | 1 +";

        for style in [
            CommitStyle::SingleLine,
            CommitStyle::Detailed,
            CommitStyle::Auto,
        ] {
            let prompt = build_prompt(diff, diff_stat, style, false);
            assert!(prompt.contains("type(scope): description"));
            assert!(prompt.contains("72 characters"));
            assert!(prompt.contains("intent"));
            assert!(prompt.contains("Output ONLY"));
        }
    }

    #[test]
    fn test_prompt_truncation_notice() {
        let diff = "diff --git a/file.rs";
        let diff_stat = "file.rs | 10 ++++++++++";

        // Without truncation
        let prompt = build_prompt(diff, diff_stat, CommitStyle::SingleLine, false);
        assert!(!prompt.contains("truncated"));

        // With truncation
        let prompt = build_prompt(diff, diff_stat, CommitStyle::SingleLine, true);
        assert!(prompt.contains("truncated"));
        assert!(prompt.contains("Focus on the visible changes"));
    }

    #[test]
    fn test_strip_markdown_code_block_basic() {
        let input = "```\nfeat: add feature\n```";
        assert_eq!(strip_markdown_code_block(input), "feat: add feature");
    }

    #[test]
    fn test_strip_markdown_code_block_with_language() {
        let input = "```text\nfeat: add feature\n```";
        assert_eq!(strip_markdown_code_block(input), "feat: add feature");
    }

    #[test]
    fn test_strip_markdown_code_block_multiline() {
        let input = "```\nfeat: add feature\n\n- Added X\n- Fixed Y\n```";
        assert_eq!(
            strip_markdown_code_block(input),
            "feat: add feature\n\n- Added X\n- Fixed Y"
        );
    }

    #[test]
    fn test_strip_markdown_code_block_no_block() {
        let input = "feat: add feature";
        assert_eq!(strip_markdown_code_block(input), "feat: add feature");
    }

    #[test]
    fn test_strip_markdown_code_block_partial() {
        // Only opening or only closing - should not strip
        let input = "```feat: add feature";
        assert_eq!(strip_markdown_code_block(input), "```feat: add feature");
    }

    #[test]
    fn test_lowercase_scope_basic() {
        let input = "docs(README): refine feature descriptions";
        assert_eq!(
            lowercase_scope(input),
            "docs(readme): refine feature descriptions"
        );
    }

    #[test]
    fn test_lowercase_scope_breaking_change() {
        let input = "feat(API)!: drop legacy endpoint";
        assert_eq!(lowercase_scope(input), "feat(api)!: drop legacy endpoint");
    }

    #[test]
    fn test_lowercase_scope_already_lowercase() {
        let input = "fix(auth): handle token refresh";
        assert_eq!(lowercase_scope(input), input);
    }

    #[test]
    fn test_lowercase_scope_no_scope() {
        let input = "feat: add feature";
        assert_eq!(lowercase_scope(input), input);
    }

    #[test]
    fn test_lowercase_scope_multiline() {
        let input = "docs(README): update usage\n\n- Added example";
        let expected = "docs(readme): update usage\n\n- Added example";
        assert_eq!(lowercase_scope(input), expected);
    }

    #[test]
    fn test_lowercase_scope_ignores_non_first_line() {
        let input = "feat: add feature\n\n- follow-up(auth): text";
        assert_eq!(lowercase_scope(input), input);
    }
}
