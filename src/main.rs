mod resolve;
mod slack;
mod spotify;
mod types;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use dashmap::DashMap;
use resolve::{extract_urls, resolve_to_spotify_track_id};
use serde_json::{json, Value};
use slack::SlackWebClient;
use spotify::SpotifyClient;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use types::SlackEnvelope;

#[derive(Clone)]
struct AppState {
    slack: Arc<SlackWebClient>,
    spotify: Option<Arc<SpotifyClient>>,
    config: Config,
    dedupe: Arc<DashMap<String, Instant>>,
    dry_run: bool,
}

#[derive(Clone)]
struct Config {
    signing_secret: String,
    music_channel_id: String,
}

#[tokio::main]
async fn main() {
    // Load environment variables
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jamcraft=info".into()),
        )
        .init();

    // Read configuration
    let bot_token =
        std::env::var("SLACK_BOT_TOKEN").expect("SLACK_BOT_TOKEN must be set in .env file");
    let signing_secret = std::env::var("SLACK_SIGNING_SECRET")
        .expect("SLACK_SIGNING_SECRET must be set in .env file");

    // Spotify credentials (required for full functionality)
    let spotify_client_id = std::env::var("SPOTIFY_CLIENT_ID").unwrap_or_else(|_| {
        eprintln!("\n⚠️  WARNING: SPOTIFY_CLIENT_ID not set");
        eprintln!("   The bot will start but won't be able to add tracks to Spotify.");
        eprintln!("   Get credentials from: https://developer.spotify.com/dashboard\n");
        String::new()
    });
    let spotify_client_secret = std::env::var("SPOTIFY_CLIENT_SECRET").unwrap_or_else(|_| {
        eprintln!("⚠️  WARNING: SPOTIFY_CLIENT_SECRET not set\n");
        String::new()
    });
    let spotify_refresh_token = std::env::var("SPOTIFY_REFRESH_TOKEN").unwrap_or_else(|_| {
        eprintln!("⚠️  WARNING: SPOTIFY_REFRESH_TOKEN not set");
        eprintln!("   Run: cargo run --bin spotify_auth\n");
        String::new()
    });
    let spotify_playlist_id = std::env::var("SPOTIFY_PLAYLIST_ID").unwrap_or_else(|_| {
        eprintln!("⚠️  WARNING: SPOTIFY_PLAYLIST_ID not set\n");
        String::new()
    });
    let music_channel_name =
        std::env::var("MUSIC_CHANNEL_NAME").unwrap_or_else(|_| "jamcraft".to_string());
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u16>()
        .expect("PORT must be a valid u16");
    let dry_run = std::env::var("DRY_RUN")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);
    let scan_existing_on_startup = std::env::var("SCAN_EXISTING_ON_STARTUP")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    if dry_run {
        warn!("DRY_RUN mode enabled - tracks will NOT be added to Spotify");
    }

    // Initialize clients
    let slack_client = Arc::new(SlackWebClient::new(bot_token));

    // Only initialize Spotify client if credentials are provided
    let spotify_client = if spotify_client_id.is_empty()
        || spotify_client_secret.is_empty()
        || spotify_refresh_token.is_empty()
        || spotify_playlist_id.is_empty()
    {
        warn!("Spotify credentials incomplete - bot will run but won't add tracks to Spotify");
        None
    } else {
        Some(Arc::new(SpotifyClient::new(
            spotify_client_id,
            spotify_client_secret,
            spotify_refresh_token,
            spotify_playlist_id,
        )))
    };

    // Resolve channel ID (with timeout to avoid blocking server startup)
    info!("Resolving channel ID for #{}", music_channel_name);
    let music_channel_id = tokio::time::timeout(
        Duration::from_secs(10),
        slack_client.resolve_channel_id_by_name(&music_channel_name),
    )
    .await;

    let music_channel_id = match music_channel_id {
        Ok(Ok(Some(id))) => {
            info!("Found channel ID: {}", id);
            id
        }
        Ok(Ok(None)) => {
            error!("Channel #{} not found", music_channel_name);
            std::process::exit(1);
        }
        Ok(Err(e)) => {
            error!("Failed to resolve channel: {}", e);
            std::process::exit(1);
        }
        Err(_) => {
            error!("Channel resolution timed out after 10 seconds");
            std::process::exit(1);
        }
    };

    let config = Config {
        signing_secret,
        music_channel_id,
    };

    let state = AppState {
        slack: slack_client,
        spotify: spotify_client,
        config,
        dedupe: Arc::new(DashMap::new()),
        dry_run,
    };

    // Cleanup old dedupe entries periodically
    let dedupe_cleanup = state.dedupe.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300)); // Every 5 minutes
        loop {
            interval.tick().await;
            let now = Instant::now();
            dedupe_cleanup.retain(|_, &mut timestamp| {
                now.duration_since(timestamp) < Duration::from_secs(3600)
            });
        }
    });

    // Optional: scan existing channel messages and add tracks to playlist
    if scan_existing_on_startup {
        let backfill_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = backfill_existing_messages(backfill_state).await {
                error!("Backfill failed: {}", e);
            }
        });
    }

    // Build router
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/slack/events", post(slack_events_handler))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    // Start server
    let addr = format!("0.0.0.0:{}", port);
    info!("Starting server on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health_handler() -> &'static str {
    "ok"
}

async fn backfill_existing_messages(state: AppState) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting backfill: scanning existing messages in #{}", state.config.music_channel_id);

    let texts = state
        .slack
        .fetch_channel_messages(&state.config.music_channel_id)
        .await
        .map_err(|e| format!("Failed to fetch channel history: {}", e))?;

    let mut seen_track_ids = std::collections::HashSet::new();
    let mut resolved_count = 0;
    let mut added_count = 0;

    for text in &texts {
        let urls = extract_urls(text);
        for url in urls {
            if let Some(track_id) = resolve_to_spotify_track_id(&url).await {
                resolved_count += 1;
                if seen_track_ids.contains(&track_id) {
                    continue;
                }
                seen_track_ids.insert(track_id.clone());

                let spotify_client = match &state.spotify {
                    Some(c) => c,
                    None => continue,
                };

                if state.dry_run {
                    info!("[DRY RUN] Would add track from backfill: {}", track_id);
                    added_count += 1;
                } else {
                    match spotify_client.add_track(&track_id).await {
                        Ok(()) => {
                            added_count += 1;
                            state.dedupe.insert(track_id, Instant::now());
                        }
                        Err(e) => {
                            warn!("Failed to add track {} during backfill: {}", track_id, e);
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    info!(
        "Backfill complete: {} messages scanned, {} tracks resolved, {} added to playlist",
        texts.len(),
        resolved_count,
        added_count
    );
    Ok(())
}

async fn slack_events_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, StatusCode> {
    info!(
        "Received request to /slack/events, body length: {} bytes",
        body.len()
    );

    // Parse JSON first to check if it's a URL verification challenge
    // (we need to respond to challenges even if signature verification fails)
    let envelope: SlackEnvelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to parse Slack envelope: {}", e);
            return Ok(Json(json!({})));
        }
    };

    // Handle URL verification immediately (before signature check)
    // Slack requires this for initial endpoint verification
    if envelope.event_type == "url_verification" {
        if let Some(challenge) = envelope.challenge {
            info!("Received URL verification challenge: {}", challenge);
            // For url_verification, we should still verify signature if headers are present
            // But we respond to the challenge regardless to allow Slack to verify the endpoint
            if let (Some(timestamp), Some(signature)) = (
                headers
                    .get("X-Slack-Request-Timestamp")
                    .and_then(|h| h.to_str().ok()),
                headers
                    .get("X-Slack-Signature")
                    .and_then(|h| h.to_str().ok()),
            ) {
                // Try to verify, but don't fail if it doesn't match (for initial setup)
                if SlackWebClient::verify_signature(
                    &state.config.signing_secret,
                    timestamp,
                    signature,
                    &body,
                )
                .is_err()
                {
                    warn!("Signature verification failed for url_verification, but responding to challenge anyway");
                } else {
                    info!("Signature verification passed for url_verification");
                }
            } else {
                warn!("Missing signature headers for url_verification");
            }
            let response = json!({ "challenge": challenge });
            info!("Responding to challenge with: {}", response);
            return Ok(Json(response));
        } else {
            warn!("url_verification event but no challenge field");
        }
    }

    // For all other events, verify signature
    let timestamp = headers
        .get("X-Slack-Request-Timestamp")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    let signature = headers
        .get("X-Slack-Signature")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    SlackWebClient::verify_signature(&state.config.signing_secret, timestamp, signature, &body)
        .map_err(|e| {
            warn!("Signature verification failed: {:?}", e);
            e
        })?;

    // Handle event callback
    if envelope.event_type == "event_callback" {
        if let Some(event) = envelope.event {
            // Ignore bot messages and subtypes
            if event.bot_id.is_some() || event.subtype.is_some() {
                return Ok(Json(json!({})));
            }

            // Check channel matches
            if let Some(ref channel) = event.channel {
                if channel != &state.config.music_channel_id {
                    return Ok(Json(json!({})));
                }
            } else {
                return Ok(Json(json!({})));
            }

            // Process message
            if let Some(text) = event.text {
                if let Some(ts) = event.ts {
                    if let Some(channel) = event.channel {
                        tokio::spawn(async move {
                            if let Err(e) =
                                process_message(state.clone(), &channel, &ts, &text).await
                            {
                                error!("Error processing message: {}", e);
                            }
                        });
                    }
                }
            }
        }
    }

    Ok(Json(json!({})))
}

async fn process_message(
    state: AppState,
    channel: &str,
    thread_ts: &str,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Extract URLs
    let urls = extract_urls(text);
    if urls.is_empty() {
        return Ok(());
    }

    // Resolve to Spotify track IDs
    let mut track_ids = Vec::new();
    for url in &urls {
        info!("Attempting to resolve URL: {}", url);
        if let Some(track_id) = resolve_to_spotify_track_id(url).await {
            info!("Successfully resolved {} to track ID: {}", url, track_id);
            track_ids.push(track_id);
        } else {
            warn!("Failed to resolve URL: {}", url);
        }
    }

    if track_ids.is_empty() {
        // Couldn't resolve any track
        state
            .slack
            .reactions_add(channel, thread_ts, "grey_question")
            .await
            .map_err(|e| format!("Failed to add reaction: {}", e))?;

        state
            .slack
            .chat_post_message(
                channel,
                Some(thread_ts),
                "Couldn't resolve that link—try a Spotify link or include artist + title.",
            )
            .await
            .map_err(|e| format!("Failed to post message: {}", e))?;

        return Ok(());
    }

    // Check if Spotify is configured
    let spotify_client = match &state.spotify {
        Some(client) => client,
        None => {
            warn!("Spotify not configured - cannot add tracks to playlist");
            state
                .slack
                .reactions_add(channel, thread_ts, "grey_question")
                .await
                .map_err(|e| format!("Failed to add reaction: {}", e))?;

            state
                .slack
                .chat_post_message(
                    channel,
                    Some(thread_ts),
                    "Spotify is not configured. Please set SPOTIFY_CLIENT_ID, SPOTIFY_CLIENT_SECRET, SPOTIFY_REFRESH_TOKEN, and SPOTIFY_PLAYLIST_ID in your .env file.",
                )
                .await
                .map_err(|e| format!("Failed to post message: {}", e))?;

            return Ok(());
        }
    };

    // Dedupe and add tracks
    let now = Instant::now();
    let mut added_count = 0;
    let mut failed_count = 0;

    for track_id in track_ids {
        // Check dedupe
        if let Some(existing) = state.dedupe.get(&track_id) {
            if now.duration_since(*existing) < Duration::from_secs(3600) {
                continue; // Skip if seen in last hour
            }
        }

        // Add to playlist (or simulate in dry-run mode)
        if state.dry_run {
            info!("[DRY RUN] Would add track: {}", track_id);
            state.dedupe.insert(track_id.clone(), now);
            added_count += 1;
        } else {
            match spotify_client.add_track(&track_id).await {
                Ok(()) => {
                    state.dedupe.insert(track_id.clone(), now);
                    added_count += 1;
                }
                Err(e) => {
                    warn!("Failed to add track {}: {}", track_id, e);
                    failed_count += 1;
                }
            }
        }
    }

    if added_count > 0 {
        // Success
        state
            .slack
            .reactions_add(channel, thread_ts, "musical_note")
            .await
            .map_err(|e| format!("Failed to add reaction: {}", e))?;

        let message = format!("Added {} track(s) to the playlist ✅", added_count);
        state
            .slack
            .chat_post_message(channel, Some(thread_ts), &message)
            .await
            .map_err(|e| format!("Failed to post message: {}", e))?;
    } else if failed_count > 0 {
        // Add attempts failed (e.g. 403)
        state
            .slack
            .reactions_add(channel, thread_ts, "grey_question")
            .await
            .map_err(|e| format!("Failed to add reaction: {}", e))?;

        state
            .slack
            .chat_post_message(
                channel,
                Some(thread_ts),
                "Couldn't add track(s) to the playlist—Spotify returned an error. If this keeps happening, try running the bot locally (Spotify may block cloud servers).",
            )
            .await
            .map_err(|e| format!("Failed to post message: {}", e))?;
    } else {
        // All tracks were duplicates
        state
            .slack
            .reactions_add(channel, thread_ts, "grey_question")
            .await
            .map_err(|e| format!("Failed to add reaction: {}", e))?;

        state
            .slack
            .chat_post_message(
                channel,
                Some(thread_ts),
                "All tracks were already added in the last hour.",
            )
            .await
            .map_err(|e| format!("Failed to post message: {}", e))?;
    }

    Ok(())
}
