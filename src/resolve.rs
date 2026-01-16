use regex::Regex;
use std::sync::LazyLock;

// Match URLs - will include trailing punctuation which is fine for most cases
static URL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https?://[^\s]+").expect("Invalid URL regex")
});

static SPOTIFY_TRACK_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"open\.spotify\.com/track/([a-zA-Z0-9]+)").expect("Invalid Spotify regex")
});

pub fn extract_urls(text: &str) -> Vec<String> {
    URL_REGEX
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

pub fn parse_spotify_track_id(url: &str) -> Option<String> {
    SPOTIFY_TRACK_REGEX
        .captures(url)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

pub async fn resolve_via_odesli(url: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let encoded_url = urlencoding::encode(url);
    let api_url = format!("https://api.song.link/v1-alpha.1/links?url={}", encoded_url);

    match client.get(&api_url).send().await {
        Ok(response) => {
            // Read response as text first (can be used for both JSON and text search)
            if let Ok(text) = response.text().await {
                // Try parsing as JSON first (preferred method)
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    // Odesli returns linksByPlatform with platform keys
                    if let Some(links) = json.get("linksByPlatform") {
                        if let Some(spotify) = links.get("spotify") {
                            if let Some(spotify_url) = spotify.get("url").and_then(|u| u.as_str()) {
                                if let Some(track_id) = parse_spotify_track_id(spotify_url) {
                                    return Some(track_id);
                                }
                            }
                        }
                    }
                }
                
                // Fallback: search text directly for Spotify URLs
                if let Some(track_id) = parse_spotify_track_id(&text) {
                    return Some(track_id);
                }
            }
        }
        Err(e) => {
            tracing::warn!("Odesli API error: {}", e);
        }
    }
    None
}

pub async fn resolve_to_spotify_track_id(url: &str) -> Option<String> {
    // Try direct Spotify parse first
    if let Some(track_id) = parse_spotify_track_id(url) {
        return Some(track_id);
    }

    // Fall back to Odesli
    resolve_via_odesli(url).await
}
