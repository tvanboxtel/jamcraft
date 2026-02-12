use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;

#[derive(Clone)]
struct TokenCache {
    access_token: String,
    expires_at: Instant,
}

pub struct SpotifyClient {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    playlist_id: String,
    client: reqwest::Client,
    token_cache: Arc<Mutex<Option<TokenCache>>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Debug, Serialize)]
struct AddTracksRequest {
    uris: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SpotifyApiErrorResponse {
    error: Option<SpotifyErrorDetail>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SpotifyErrorDetail {
    status: Option<u16>,
    message: Option<String>,
}

#[derive(Debug)]
pub enum SpotifyError {
    Network(String),
    Auth(String),
    RateLimit(u64),
    Api(String),
    #[allow(dead_code)]
    Other(String),
}

impl std::fmt::Display for SpotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpotifyError::Network(msg) => write!(f, "Network error: {}", msg),
            SpotifyError::Auth(msg) => write!(f, "Auth error: {}", msg),
            SpotifyError::RateLimit(secs) => write!(f, "Rate limited, retry after {}s", secs),
            SpotifyError::Api(msg) => write!(f, "API error: {}", msg),
            SpotifyError::Other(msg) => write!(f, "Error: {}", msg),
        }
    }
}

impl std::error::Error for SpotifyError {}

impl SpotifyClient {
    pub fn new(
        client_id: String,
        client_secret: String,
        refresh_token: String,
        playlist_id: String,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            refresh_token,
            playlist_id,
            client: reqwest::Client::new(),
            token_cache: Arc::new(Mutex::new(None)),
        }
    }

    async fn get_access_token(&self) -> Result<String, SpotifyError> {
        // Check cache first
        {
            let cache = self.token_cache.lock().unwrap();
            if let Some(ref token_cache) = *cache {
                if token_cache.expires_at > Instant::now() {
                    return Ok(token_cache.access_token.clone());
                }
            }
        }

        // Refresh token
        let auth = BASE64_STANDARD.encode(format!("{}:{}", self.client_id, self.client_secret));
        let url = "https://accounts.spotify.com/api/token";

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", &self.refresh_token),
        ];

        let response = self
            .client
            .post(url)
            .header("Authorization", format!("Basic {}", auth))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params)
            .send()
            .await
            .map_err(|e| SpotifyError::Network(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(SpotifyError::Auth(format!(
                "Token refresh failed: {} - {}",
                status, text
            )));
        }

        let token_response: TokenResponse = response
            .json()
            .await
            .map_err(|e| SpotifyError::Network(format!("Parse failed: {}", e)))?;

        // Cache the token (subtract 60 seconds for safety margin)
        let expires_at =
            Instant::now() + Duration::from_secs(token_response.expires_in.saturating_sub(60));
        let cache = TokenCache {
            access_token: token_response.access_token.clone(),
            expires_at,
        };

        {
            let mut token_cache = self.token_cache.lock().unwrap();
            *token_cache = Some(cache);
        }

        Ok(token_response.access_token)
    }

    /// Fetches all track IDs currently in the playlist. Requires playlist-read-private scope.
    pub async fn get_playlist_track_ids(
        &self,
    ) -> Result<std::collections::HashSet<String>, SpotifyError> {
        let mut track_ids = std::collections::HashSet::new();
        let mut offset = 0;
        let limit = 50;

        loop {
            let access_token = self.get_access_token().await?;
            let url = format!(
                "https://api.spotify.com/v1/playlists/{}/items?limit={}&offset={}",
                self.playlist_id, limit, offset
            );

            let response = self
                .client
                .get(&url)
                .header("Authorization", format!("Bearer {}", access_token))
                .send()
                .await
                .map_err(|e| SpotifyError::Network(format!("Request failed: {}", e)))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(SpotifyError::Api(format!(
                    "Get playlist items failed: {} - {}",
                    status, text
                )));
            }

            let json: serde_json::Value = response
                .json()
                .await
                .map_err(|e| SpotifyError::Network(format!("Parse failed: {}", e)))?;

            let items = json
                .get("items")
                .and_then(|i| i.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            for item in items {
                if let Some(item_obj) = item.get("item") {
                    if item_obj.get("type").and_then(|t| t.as_str()) == Some("track") {
                        if let Some(id) = item_obj.get("id").and_then(|i| i.as_str()) {
                            track_ids.insert(id.to_string());
                        }
                    }
                }
            }

            let total = json.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
            offset += items.len() as u32;
            if offset as u64 >= total || items.is_empty() {
                break;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(track_ids)
    }

    pub async fn add_track(&self, track_id: &str) -> Result<(), SpotifyError> {
        let mut can_retry_auth = true;
        let mut can_retry_rate_limit = true;

        loop {
            let access_token = self.get_access_token().await?;

            let url = format!(
                "https://api.spotify.com/v1/playlists/{}/items",
                self.playlist_id
            );

            let payload = AddTracksRequest {
                uris: vec![format!("spotify:track:{}", track_id)],
            };

            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", access_token))
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await
                .map_err(|e| SpotifyError::Network(format!("Request failed: {}", e)))?;

            let status = response.status();

            // Handle 401: refresh and retry once
            if status == 401 && can_retry_auth {
                warn!("Got 401, clearing token cache and retrying");
                {
                    let mut cache = self.token_cache.lock().unwrap();
                    *cache = None;
                }
                can_retry_auth = false;
                continue;
            }

            // Handle 429: rate limit
            if status == 429 {
                let retry_after = response
                    .headers()
                    .get("Retry-After")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1);

                if can_retry_rate_limit {
                    warn!("Rate limited, waiting {} seconds", retry_after);
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;
                    can_retry_rate_limit = false;
                    continue;
                }
                return Err(SpotifyError::RateLimit(retry_after));
            }

            if !status.is_success() {
                let headers: Vec<_> = response
                    .headers()
                    .iter()
                    .filter(|(k, _)| {
                        let k = k.as_str().to_lowercase();
                        k.starts_with("x-") || k.starts_with("spotify-") || k == "retry-after"
                    })
                    .map(|(k, v)| format!("{}: {:?}", k.as_str(), v.to_str().unwrap_or("?")))
                    .collect();
                let header_info = if headers.is_empty() {
                    String::new()
                } else {
                    format!(" Headers: {:?}", headers)
                };
                let text = response.text().await.unwrap_or_default();
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    let err_obj = json.get("error");
                    let msg = err_obj
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("(no message)");
                    let reason = err_obj
                        .and_then(|e| e.get("reason"))
                        .and_then(|r| r.as_str());
                    let mut detail = format!(
                        "Spotify API error: status={} playlist_id={} track_id={} message={}",
                        status, self.playlist_id, track_id, msg
                    );
                    if let Some(r) = reason {
                        detail.push_str(&format!(" reason={}", r));
                    }
                    detail.push_str(&header_info);
                    warn!("{}", detail);
                }
                return Err(SpotifyError::Api(format!(
                    "Add track failed: {} - {}",
                    status, text
                )));
            }

            return Ok(());
        }
    }
}

impl SpotifyClient {
    /// Search for a track by artist and title. Returns the best match track ID if found.
    pub async fn search_track(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Option<String>, SpotifyError> {
        let access_token = self.get_access_token().await?;

        let query = format!(
            "artist:\"{}\" track:\"{}\"",
            artist.replace('"', "\\\""),
            title.replace('"', "\\\"")
        );
        let encoded = urlencoding::encode(&query);
        let url = format!(
            "https://api.spotify.com/v1/search?q={}&type=track&limit=1",
            encoded
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| SpotifyError::Network(format!("Search failed: {}", e)))?;

        if !response.status().is_success() {
            tracing::warn!("Spotify search failed: {}", response.status());
            return Ok(None);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| SpotifyError::Network(format!("Parse failed: {}", e)))?;

        let track_id = json
            .get("tracks")
            .and_then(|t| t.get("items"))
            .and_then(|i| i.as_array())
            .and_then(|a| a.first())
            .and_then(|t| t.get("id"))
            .and_then(|id| id.as_str())
            .map(|s| s.to_string());

        if let Some(ref id) = track_id {
            tracing::info!("Spotify search found: {} - {} -> {}", artist, title, id);
        }
        Ok(track_id)
    }
}
