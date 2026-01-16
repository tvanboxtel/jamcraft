use axum::http::StatusCode;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

pub struct SlackWebClient {
    bot_token: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct SlackApiResponse<T> {
    ok: bool,
    #[serde(flatten)]
    data: T,
}

#[derive(Debug, Deserialize)]
struct ConversationsListResponse {
    channels: Vec<Channel>,
    #[serde(rename = "response_metadata")]
    response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
struct Channel {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct ResponseMetadata {
    #[serde(rename = "next_cursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReactionsAddRequest {
    channel: String,
    timestamp: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct ChatPostMessageRequest {
    channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<String>,
    text: String,
}

impl SlackWebClient {
    pub fn new(bot_token: String) -> Self {
        Self {
            bot_token,
            client: reqwest::Client::new(),
        }
    }

    pub fn verify_signature(
        signing_secret: &str,
        timestamp: &str,
        signature: &str,
        raw_body: &[u8],
    ) -> Result<(), StatusCode> {
        // Parse timestamp
        let ts: u64 = timestamp.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

        // Check timestamp is within 5 minutes
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .as_secs();

        if now.abs_diff(ts) > 300 {
            return Err(StatusCode::UNAUTHORIZED);
        }

        // Build base string
        let body_str = String::from_utf8_lossy(raw_body);
        let base_string = format!("v0:{}:{}", ts, body_str);

        // Compute HMAC
        let mut mac = HmacSha256::new_from_slice(signing_secret.as_bytes())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        mac.update(base_string.as_bytes());
        let result = mac.finalize();
        let computed = format!("v0={}", hex::encode(result.into_bytes()));

        // Constant-time comparison
        if !constant_time_eq(signature.as_bytes(), computed.as_bytes()) {
            return Err(StatusCode::UNAUTHORIZED);
        }

        Ok(())
    }

    pub async fn reactions_add(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), String> {
        let url = "https://slack.com/api/reactions.add";
        let payload = ReactionsAddRequest {
            channel: channel.to_string(),
            timestamp: timestamp.to_string(),
            name: name.to_string(),
        };

        let response: SlackApiResponse<HashMap<String, serde_json::Value>> = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?
            .json()
            .await
            .map_err(|e| format!("Parse failed: {}", e))?;

        if !response.ok {
            return Err(format!("Slack API error: {:?}", response.data));
        }

        Ok(())
    }

    pub async fn chat_post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), String> {
        let url = "https://slack.com/api/chat.postMessage";
        let payload = ChatPostMessageRequest {
            channel: channel.to_string(),
            thread_ts: thread_ts.map(|s| s.to_string()),
            text: text.to_string(),
        };

        let response: SlackApiResponse<HashMap<String, serde_json::Value>> = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?
            .json()
            .await
            .map_err(|e| format!("Parse failed: {}", e))?;

        if !response.ok {
            return Err(format!("Slack API error: {:?}", response.data));
        }

        Ok(())
    }

    pub async fn resolve_channel_id_by_name(&self, channel_name: &str) -> Result<Option<String>, String> {
        let url = "https://slack.com/api/conversations.list";
        let mut cursor: Option<String> = None;
        let max_pages = 5;
        let mut page_count = 0;

        loop {
            if page_count >= max_pages {
                break;
            }
            page_count += 1;

            let mut params = vec![
                ("limit", "200"),
                ("types", "public_channel"),
            ];

            if let Some(ref c) = cursor {
                params.push(("cursor", c));
            }

            // Parse response as raw JSON first to check 'ok' field
            let raw_response: serde_json::Value = self
                .client
                .get(url)
                .header("Authorization", format!("Bearer {}", self.bot_token))
                .query(&params)
                .send()
                .await
                .map_err(|e| format!("Request failed: {}", e))?
                .json()
                .await
                .map_err(|e| format!("Parse failed: {}", e))?;

            // Check if request was successful
            let ok = raw_response.get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if !ok {
                let error_msg = raw_response.get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Unknown error");
                
                // Check for missing scope details
                let mut error_details = format!("Slack API error: {}", error_msg);
                if error_msg == "missing_scope" {
                    if let Some(needed) = raw_response.get("needed") {
                        error_details.push_str(&format!(" (needed: {:?})", needed));
                    }
                    if let Some(provided) = raw_response.get("provided") {
                        error_details.push_str(&format!(" (provided: {:?})", provided));
                    }
                }
                
                return Err(error_details);
            }

            // Now parse as ConversationsListResponse
            let response: ConversationsListResponse = serde_json::from_value(raw_response)
                .map_err(|e| format!("Failed to parse response: {}", e))?;

            // Search for matching channel
            for channel in &response.channels {
                if channel.name == channel_name {
                    return Ok(Some(channel.id.clone()));
                }
            }

            // Check for next page
            cursor = response
                .response_metadata
                .and_then(|m| m.next_cursor)
                .filter(|c| !c.is_empty());

            if cursor.is_none() {
                break;
            }
        }

        Ok(None)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}
