use axum::{extract::Query, response::Html, routing::get, Router};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct CallbackParams {
    code: Option<String>,
    error: Option<String>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let client_id = match std::env::var("SPOTIFY_CLIENT_ID") {
        Ok(id) => id,
        Err(_) => {
            eprintln!("\n❌ SPOTIFY_CLIENT_ID is not set in your .env file");
            eprintln!("\nTo get your Spotify credentials:");
            eprintln!("1. Go to https://developer.spotify.com/dashboard");
            eprintln!("2. Create a new app");
            eprintln!("3. Copy the Client ID and Client Secret");
            eprintln!("4. Add them to your .env file:");
            eprintln!("   SPOTIFY_CLIENT_ID=your_client_id");
            eprintln!("   SPOTIFY_CLIENT_SECRET=your_client_secret");
            eprintln!("\nOnce the Spotify dashboard is back up, run this script again.\n");
            std::process::exit(1);
        }
    };

    let redirect_uri = "http://127.0.0.1:3000/spotify/callback";
    let scope = "playlist-modify-public playlist-modify-private";

    let auth_url = format!(
        "https://accounts.spotify.com/authorize?client_id={}&response_type=code&redirect_uri={}&scope={}",
        client_id,
        urlencoding::encode(redirect_uri),
        urlencoding::encode(scope)
    );

    println!("\n=== Spotify OAuth Setup ===\n");
    println!("1. Open this URL in your browser:");
    println!("   {}\n", auth_url);
    println!("2. Authorize the app and copy the code from the callback URL\n");
    println!("Waiting for callback on http://127.0.0.1:3000/spotify/callback...\n");

    let app = Router::new().route("/spotify/callback", get(callback_handler));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn callback_handler(Query(params): Query<CallbackParams>) -> Html<String> {
    if let Some(error) = params.error {
        return Html(format!(
            r#"
            <html>
                <body>
                    <h1>Authorization Error</h1>
                    <p>Error: {}</p>
                </body>
            </html>
            "#,
            error
        ));
    }

    if let Some(code) = params.code {
        match exchange_code(&code).await {
            Ok(refresh_token) => {
                println!("\n=== SUCCESS ===\n");
                println!("Add this to your .env file:");
                println!("SPOTIFY_REFRESH_TOKEN={}\n", refresh_token);
                println!("You can now close this window and stop the server (Ctrl+C).\n");

                return Html(
                    r#"
                    <html>
                        <body>
                            <h1>Success!</h1>
                            <p>Check your terminal for the refresh token.</p>
                            <p>You can close this window.</p>
                        </body>
                    </html>
                "#
                    .to_string(),
                );
            }
            Err(e) => {
                eprintln!("Error exchanging code: {}", e);
                return Html(format!(
                    r#"
                    <html>
                        <body>
                            <h1>Error</h1>
                            <p>Failed to exchange code: {}</p>
                            <p>Check the terminal for details.</p>
                        </body>
                    </html>
                    "#,
                    e
                ));
            }
        }
    }

    Html(
        r#"
        <html>
            <body>
                <h1>No code received</h1>
                <p>Please try again.</p>
            </body>
        </html>
    "#
        .to_string(),
    )
}

async fn exchange_code(code: &str) -> Result<String, String> {
    let client_id = std::env::var("SPOTIFY_CLIENT_ID")
        .map_err(|_| "SPOTIFY_CLIENT_ID not set in environment")?;
    let client_secret = std::env::var("SPOTIFY_CLIENT_SECRET")
        .map_err(|_| "SPOTIFY_CLIENT_SECRET not set in environment")?;

    let redirect_uri = "http://127.0.0.1:3000/spotify/callback";
    let auth = BASE64_STANDARD.encode(format!("{}:{}", client_id, client_secret));

    let client = reqwest::Client::new();
    let mut params = HashMap::new();
    params.insert("grant_type", "authorization_code");
    params.insert("code", code);
    params.insert("redirect_uri", redirect_uri);

    let response = client
        .post("https://accounts.spotify.com/api/token")
        .header("Authorization", format!("Basic {}", auth))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {} - {}", status, text));
    }

    #[derive(serde::Deserialize)]
    struct TokenResponse {
        refresh_token: String,
    }

    let token_response: TokenResponse = response
        .json()
        .await
        .map_err(|e| format!("Parse failed: {}", e))?;

    Ok(token_response.refresh_token)
}
