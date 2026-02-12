// Quick diagnostic: test Spotify token and add-track endpoint
// Run: cargo run --bin spotify_check

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let client_id = std::env::var("SPOTIFY_CLIENT_ID").expect("SPOTIFY_CLIENT_ID");
    let client_secret = std::env::var("SPOTIFY_CLIENT_SECRET").expect("SPOTIFY_CLIENT_SECRET");
    let refresh_token = std::env::var("SPOTIFY_REFRESH_TOKEN").expect("SPOTIFY_REFRESH_TOKEN");
    let playlist_id = std::env::var("SPOTIFY_PLAYLIST_ID").expect("SPOTIFY_PLAYLIST_ID");

    let client = reqwest::Client::new();

    // 1. Get access token
    println!("1. Refreshing token...");
    let auth = BASE64_STANDARD.encode(format!("{}:{}", client_id, client_secret));
    let token_resp = client
        .post("https://accounts.spotify.com/api/token")
        .header("Authorization", format!("Basic {}", auth))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
        ])
        .send()
        .await
        .expect("token request failed");

    let token_status = token_resp.status();
    let token_body = token_resp.text().await.unwrap_or_default();

    if !token_status.is_success() {
        println!(
            "   FAIL: Token refresh returned {}:\n{}",
            token_status, token_body
        );
        return;
    }

    let token_json: serde_json::Value = serde_json::from_str(&token_body).expect("parse token");
    let access_token = token_json["access_token"].as_str().expect("access_token");
    let scopes = token_json["scope"].as_str().unwrap_or("(not in response)");
    println!("   OK. Scopes in token: {}", scopes);

    // 2. Get current user (verify token works)
    println!("\n2. Getting current user (GET /v1/me)...");
    let me_resp = client
        .get("https://api.spotify.com/v1/me")
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .expect("me request failed");

    let me_status = me_resp.status();
    let me_body = me_resp.text().await.unwrap_or_default();

    if !me_status.is_success() {
        println!("   FAIL: {} - {}", me_status, me_body);
        return;
    }
    let me_json: serde_json::Value = serde_json::from_str(&me_body).expect("parse me");
    let user_id = me_json["id"].as_str().unwrap_or("?");
    println!(
        "   OK. User: {} ({})",
        me_json["display_name"].as_str().unwrap_or("?"),
        user_id
    );

    // 2b. Try to read the playlist (needs playlist-read-private)
    println!("\n2b. Getting playlist details (GET /v1/playlists/...)...");
    let playlist_resp = client
        .get(format!(
            "https://api.spotify.com/v1/playlists/{}",
            playlist_id
        ))
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .expect("playlist request failed");

    let playlist_status = playlist_resp.status();
    let playlist_body = playlist_resp.text().await.unwrap_or_default();

    if playlist_status.is_success() {
        let p: serde_json::Value =
            serde_json::from_str(&playlist_body).unwrap_or(serde_json::json!({}));
        let owner_id = p["owner"]["id"].as_str().unwrap_or("?");
        let playlist_name = p["name"].as_str().unwrap_or("?");
        println!("   OK. Playlist: \"{}\"", playlist_name);
        println!("       Owner ID: {} (your user ID: {})", owner_id, user_id);
        if owner_id != user_id {
            println!("       ⚠️  MISMATCH - playlist is owned by someone else! You need to be a collaborator.");
        }
    } else {
        println!(
            "   Cannot read playlist ({}): {}",
            playlist_status, playlist_body
        );
        println!("   (We may lack playlist-read scope; that's ok for the add test)");
    }

    // 3. Try to add a track
    let track_id = std::env::var("SPOTIFY_CHECK_TRACK")
        .unwrap_or_else(|_| "3n3Ppam7vgaVa1iaRUc9Lp".to_string()); // Adele - Someone Like You (globally available)
    println!(
        "\n3. Trying to add track {} to playlist {} (POST)...",
        track_id, playlist_id
    );

    let add_resp = client
        .post(format!(
            "https://api.spotify.com/v1/playlists/{}/items",
            playlist_id
        ))
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "uris": [format!("spotify:track:{}", track_id)] }))
        .send()
        .await
        .expect("add request failed");

    let add_status = add_resp.status();
    let add_headers: Vec<_> = add_resp
        .headers()
        .iter()
        .map(|(k, v)| format!("{}: {:?}", k.as_str(), v.to_str().unwrap_or("?")))
        .collect();
    let add_body = add_resp.text().await.unwrap_or_default();

    println!("   Status: {}", add_status);
    println!("   Headers: {:?}", add_headers);
    println!("   Body: {}", add_body);

    if add_status.is_success() {
        println!("\n   SUCCESS - track was added!");
    } else {
        println!("\n   FAILED - check the output above.");
    }
}
