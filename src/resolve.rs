use regex::Regex;
use std::sync::LazyLock;

// Match URLs - will include trailing punctuation which is fine for most cases
static URL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s]+").expect("Invalid URL regex"));

static SPOTIFY_TRACK_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"open\.spotify\.com/track/([a-zA-Z0-9]+)").expect("Invalid Spotify regex")
});

static QOBUZ_TRACK_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"open\.qobuz\.com/track/([a-zA-Z0-9]+)").expect("Invalid Qobuz regex")
});

pub fn extract_urls(text: &str) -> Vec<String> {
    URL_REGEX
        .find_iter(text)
        .map(|m| {
            // Clean up URL - remove trailing punctuation that might have been captured
            m.as_str()
                .trim_end_matches(|c: char| ".,;:!?)]>".contains(c))
                .to_string()
        })
        .collect()
}

pub fn parse_spotify_track_id(url: &str) -> Option<String> {
    SPOTIFY_TRACK_REGEX
        .captures(url)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn parse_qobuz_track_id(url: &str) -> Option<String> {
    QOBUZ_TRACK_REGEX
        .captures(url)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Fetch track metadata (artist, title) from Qobuz API. Uses open.qobuz.com's public
/// app_id which allows simple GET without signing - same as their smart-link pages.
pub async fn fetch_qobuz_track_metadata(track_id: &str) -> Option<(String, String)> {
    const QOBUZ_OPEN_APP_ID: &str = "712109809";

    let client = reqwest::Client::new();
    let url = format!(
        "https://www.qobuz.com/api.json/0.2/track/get?track_id={}&app_id={}",
        track_id, QOBUZ_OPEN_APP_ID
    );

    tracing::info!(
        "Qobuz fetch: track_id={} app_id={}",
        track_id,
        QOBUZ_OPEN_APP_ID
    );

    let response = client
        .get(&url)
        .header("Origin", "https://open.qobuz.com")
        .header("Referer", "https://open.qobuz.com/")
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let preview = if body.len() > 200 {
            body[..200].to_string()
        } else {
            body
        };
        tracing::warn!("Qobuz API returned {} - {}", status, preview);
        return None;
    }

    let json: serde_json::Value = response.json().await.ok()?;
    let title = json.get("title").and_then(|t| t.as_str())?.to_string();

    // Artist: try performer.name, performers[0].name, album.artist.name, composer.name
    let artist = json
        .get("performer")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .or_else(|| {
            json.get("performers")
                .and_then(|p| p.as_array())
                .and_then(|a| a.first())
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
        })
        .or_else(|| {
            json.get("album")
                .and_then(|a| a.get("artist"))
                .and_then(|ar| ar.get("name"))
                .and_then(|n| n.as_str())
        })
        .or_else(|| {
            json.get("composer")
                .and_then(|c| c.get("name"))
                .and_then(|n| n.as_str())
        })
        .map(|s| s.to_string())?;

    tracing::info!("Qobuz metadata: artist={} title={}", artist, title);
    Some((artist, title))
}

pub async fn resolve_via_odesli(url: &str) -> Option<String> {
    // Create a client that follows redirects (important for short links like link.deezer.com)
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let encoded_url = urlencoding::encode(url);
    let api_url = format!("https://api.song.link/v1-alpha.1/links?url={}", encoded_url);

    tracing::debug!("Calling Odesli API for URL: {}", url);

    match client.get(&api_url).send().await {
        Ok(response) => {
            let status = response.status();
            tracing::debug!("Odesli API response status: {}", status);

            if !status.is_success() {
                tracing::warn!("Odesli API returned non-success status: {}", status);
                return None;
            }

            // Read response as text first (can be used for both JSON and text search)
            if let Ok(text) = response.text().await {
                tracing::debug!("Odesli API response length: {} bytes", text.len());

                // Try parsing as JSON first (preferred method)
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    tracing::debug!("Successfully parsed Odesli JSON response");

                    // Odesli returns linksByPlatform with platform keys
                    if let Some(links) = json.get("linksByPlatform") {
                        if let Some(spotify) = links.get("spotify") {
                            // Try "url" field first
                            if let Some(spotify_url) = spotify.get("url").and_then(|u| u.as_str()) {
                                tracing::debug!(
                                    "Found Spotify URL in Odesli response: {}",
                                    spotify_url
                                );
                                if let Some(track_id) = parse_spotify_track_id(spotify_url) {
                                    tracing::info!(
                                        "Resolved {} to Spotify track: {}",
                                        url,
                                        track_id
                                    );
                                    return Some(track_id);
                                } else {
                                    tracing::warn!(
                                        "Could not parse track ID from Spotify URL: {}",
                                        spotify_url
                                    );
                                }
                            }
                            // Also try "entityUniqueId" or other fields that might contain the URL
                            if let Some(entity_id) =
                                spotify.get("entityUniqueId").and_then(|u| u.as_str())
                            {
                                tracing::debug!("Found Spotify entityUniqueId: {}", entity_id);
                                // entityUniqueId might be the track ID directly
                                if !entity_id.is_empty() && entity_id.len() > 10 {
                                    tracing::info!(
                                        "Using entityUniqueId as track ID: {}",
                                        entity_id
                                    );
                                    return Some(entity_id.to_string());
                                }
                            }
                            tracing::debug!(
                                "Spotify entry found but no usable URL or entityUniqueId"
                            );
                        } else {
                            tracing::debug!(
                                "No Spotify entry in linksByPlatform. Available platforms: {:?}",
                                links.as_object().map(|o| o.keys().collect::<Vec<_>>())
                            );
                        }
                    } else {
                        tracing::debug!(
                            "No linksByPlatform in Odesli response. Top-level keys: {:?}",
                            json.as_object().map(|o| o.keys().collect::<Vec<_>>())
                        );
                    }
                } else {
                    tracing::warn!("Failed to parse Odesli response as JSON");
                }

                // Fallback: search text directly for Spotify URLs
                if let Some(track_id) = parse_spotify_track_id(&text) {
                    tracing::info!(
                        "Found Spotify track ID in Odesli response text: {}",
                        track_id
                    );
                    return Some(track_id);
                }
            } else {
                tracing::warn!("Failed to read Odesli response body");
            }
        }
        Err(e) => {
            tracing::warn!("Odesli API request failed: {}", e);
        }
    }

    tracing::debug!("Could not resolve {} via Odesli", url);
    None
}

async fn resolve_short_link(url: &str) -> Option<String> {
    // For short links like link.deezer.com, resolve to the full URL first
    if url.contains("link.deezer.com") || url.contains("link.spotify.com") {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        match client.get(url).send().await {
            Ok(response) => {
                // Get the final URL after redirects
                let final_url = response.url().to_string();
                if final_url != url {
                    tracing::debug!("Resolved short link {} to {}", url, final_url);
                    return Some(final_url);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to resolve short link {}: {}", url, e);
            }
        }
    }
    None
}

fn normalize_for_odesli(url: &str) -> String {
    // Odesli may not recognize music.youtube.com; normalize to youtube.com (same video ID format)
    if url.contains("music.youtube.com") {
        return url.replace("music.youtube.com", "www.youtube.com");
    }
    url.to_string()
}

pub async fn resolve_to_spotify_track_id(url: &str) -> Option<String> {
    // Try direct Spotify parse first
    if let Some(track_id) = parse_spotify_track_id(url) {
        return Some(track_id);
    }

    // Odesli doesn't support Qobuz - skip the call, let caller use Qobuz fallback
    if parse_qobuz_track_id(url).is_some() {
        tracing::debug!("Qobuz URL detected, skipping Odesli");
        return None;
    }

    let url = normalize_for_odesli(url);

    // For short links (link.deezer.com, link.spotify.com), resolve them first
    // Odesli works better with full URLs
    let url_to_use = if url.contains("link.deezer.com") || url.contains("link.spotify.com") {
        tracing::info!("Detected short link, resolving: {}", url);
        if let Some(resolved) = resolve_short_link(&url).await {
            tracing::info!("Resolved short link {} to {}", url, resolved);
            resolved
        } else {
            tracing::warn!(
                "Failed to resolve short link {}, will try original URL with Odesli",
                url
            );
            url.to_string()
        }
    } else {
        url.to_string()
    };

    // Fall back to Odesli with the (possibly resolved) URL
    tracing::debug!("Calling Odesli with URL: {}", url_to_use);
    resolve_via_odesli(&url_to_use).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_qobuz_track_id_extracts_id() {
        assert_eq!(
            parse_qobuz_track_id("https://open.qobuz.com/track/23847392"),
            Some("23847392".to_string())
        );
        assert_eq!(parse_qobuz_track_id("https://open.spotify.com/track/abc"), None);
    }

    #[tokio::test]
    async fn fetch_qobuz_metadata_returns_artist_and_title() {
        let meta = fetch_qobuz_track_metadata("23847392").await;
        assert!(meta.is_some(), "Qobuz API should return metadata");
        let (artist, title) = meta.unwrap();
        assert_eq!(artist, "Bloc Party");
        assert_eq!(title, "Helicopter");
    }

}
