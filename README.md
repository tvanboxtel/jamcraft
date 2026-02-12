# jamcraft

A Rust Slack bot that automatically adds music links from the `#jamcraft` channel to a Spotify playlist.

## Features

- Listens to Slack Events API for messages in `#jamcraft`
- Detects Spotify, YouTube, and Deezer links
- Resolves links to Spotify track IDs (via direct parsing or Odesli/song.link API)
- Adds tracks to a Spotify playlist
- Reacts with 🎵 on success, ❓ on failure
- Replies in thread with confirmation
- In-memory deduplication (1 hour TTL) to prevent duplicate adds
- Skips tracks already in the playlist (checks Spotify before adding)
- Optional backfill: scan existing channel messages on startup to add missed tracks

## Prerequisites

- Rust 2021 edition or later
- A Slack workspace where you can create apps
- A Spotify account and developer app
- ngrok (for local development)

## Setup

### 1. Create a Slack App

1. Go to https://api.slack.com/apps
2. Click "Create New App" → "From scratch"
3. Name it (e.g., "jamcraft") and select your workspace
4. Go to **OAuth & Permissions**:
   - Add the following Bot Token Scopes:
     - `channels:read` - View basic information about public channels
     - `channels:history` - View messages in public channels
     - `chat:write` - Send messages
     - `reactions:write` - Add reactions
   - Click "Install to Workspace" (or "Reinstall to Workspace" if you added scopes) and copy the **Bot User OAuth Token** (starts with `xoxb-`)
   - **Important:** If you add scopes after initial installation, you MUST reinstall to get a new token with the updated permissions
5. Go to **Event Subscriptions**:
   - Enable Events
   - Set Request URL (use ngrok URL + `/slack/events` for local dev, see step 4)
   - Subscribe to bot events:
     - `message.channels` - Listen to messages in public channels
   - Save changes
6. Go to **Basic Information**:
   - Copy the **Signing Secret**

### 2. Create a Spotify App

1. Go to https://developer.spotify.com/dashboard
2. Click "Create app"
3. Fill in app details (name, description)
4. Check "I understand and agree to Spotify's Developer Terms of Service"
5. Click "Save"
6. In app settings, add **Redirect URIs** (you can add multiple):
   - For local dev: `http://127.0.0.1:3000/spotify/callback` (Spotify requires loopback IP, not `localhost`)
   - For production: `https://jamcraft.fly.dev/spotify/callback` (or your Fly.io app URL—see [Deployment](#deployment-flyio))
7. Copy the **Client ID** and **Client Secret**

**Note:** If the Spotify Developer Dashboard is temporarily unavailable, you can set up everything else and add Spotify credentials later. The bot will work without Spotify (it will just inform users that Spotify isn't configured).

### 3. Get Spotify Refresh Token

1. Create a `.env` file in the project root with:

   ```
   SPOTIFY_CLIENT_ID=your_client_id
   SPOTIFY_CLIENT_SECRET=your_client_secret
   ```

2. Run the auth script:

   ```bash
   cargo run --bin spotify_auth
   ```

3. Open the URL printed in your terminal in a browser
4. Authorize the app
5. Copy the `SPOTIFY_REFRESH_TOKEN` from the terminal output

### 4. Set Up ngrok (for local development)

1. Install ngrok: https://ngrok.com/download
   - Or via Homebrew: `brew install ngrok/ngrok/ngrok`
   - Or download directly from the website
2. Start ngrok:
   ```bash
   ngrok http 127.0.0.1:3000
   ```
   (Using `127.0.0.1:3000` explicitly can help avoid forwarding issues)
3. Copy the HTTPS URL from the ngrok output (e.g., `https://abc123.ngrok-free.dev`)
4. Update your Slack app's Event Subscriptions Request URL to: `https://abc123.ngrok-free.dev/slack/events`
5. Click "Save Changes" - Slack will verify the URL automatically

### 5. Configure Environment Variables

Create or update your `.env` file with all required variables:

```env
SLACK_BOT_TOKEN=xoxb-your-bot-token
SLACK_SIGNING_SECRET=your-signing-secret
SPOTIFY_CLIENT_ID=your-client-id
SPOTIFY_CLIENT_SECRET=your-client-secret
SPOTIFY_REFRESH_TOKEN=your-refresh-token
SPOTIFY_PLAYLIST_ID=your-playlist-id
PORT=3000
MUSIC_CHANNEL_NAME=jamcraft
DRY_RUN=false  # Set to "true" to test without actually adding tracks to Spotify
SCAN_EXISTING_ON_STARTUP=false  # Set to "true" to backfill existing channel messages into the playlist on startup
```

**Getting the Spotify Playlist ID:**

1. Open your Spotify playlist in the web player
2. The URL will be: `https://open.spotify.com/playlist/PLAYLIST_ID`
3. Copy the `PLAYLIST_ID` part

### 6. Invite Bot to Channel

Before running the bot, make sure to:

1. Create the `#jamcraft` channel in Slack (if it doesn't exist)
2. Invite the bot to the channel:
   - In `#jamcraft`, type: `/invite @your-bot-name`
   - Or use channel settings → Integrations → Add apps

### 7. Run the Bot

```bash
cargo run
```

The bot will:

- Resolve the `#jamcraft` channel ID at startup
- Start listening on `0.0.0.0:3000`
- Process messages in `#jamcraft` that contain music links

## Usage

### Testing Without Spotify

You can test the bot even if Spotify credentials aren't set up yet:

```bash
# Test in dry-run mode (simulates adding tracks)
DRY_RUN=true cargo run

# Or run normally (will show helpful error messages)
cargo run
```

**What you can test without Spotify:**

- ✅ Slack event reception and signature verification
- ✅ URL extraction from messages
- ✅ Spotify track ID resolution (direct parsing + Odesli API)
- ✅ Slack reactions and messages
- ✅ Channel resolution and filtering
- ✅ Deduplication logic

**What happens when Spotify isn't configured:**

- Bot will react with ❓ and reply: "Spotify is not configured..."
- All other functionality still works
- Once Spotify is set up, tracks will be added automatically

### Normal Usage

Post a message in `#jamcraft` with a music link:

- **Spotify link**: `https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT`
- **YouTube link**: `https://www.youtube.com/watch?v=dQw4w9WgXcQ`
- **Deezer link**: `https://www.deezer.com/track/123456`

The bot will:

1. Extract the URL
2. Resolve it to a Spotify track ID
3. Check if it was added in the last hour (deduplication)
4. Add it to your Spotify playlist
5. React with 🎵 and reply in thread: "Added N track(s) to the playlist ✅"

If the link can't be resolved, it will react with ❓ and reply: "Couldn't resolve that link—try a Spotify link or include artist + title."

### Backfilling Existing Messages

To add tracks from messages that were posted *before* the bot was running, set `SCAN_EXISTING_ON_STARTUP=true` in your `.env`. On startup, the bot will:

1. Fetch all messages (including thread replies) from the `#jamcraft` channel
2. Extract music links, resolve them to Spotify tracks
3. Add any new tracks to the playlist (skips duplicates within the scan)

Run this once when first deploying, or whenever you want to import older links. The scan runs in the background after the server starts. Check logs for "Backfill complete" to see how many tracks were added.

**Note:** Tracks already in the playlist from before may be added again (duplicates). You can remove them manually in Spotify if needed.

## Deployment (Fly.io)

For production deployment on Fly.io:

1. **Install flyctl**:

   ```bash
   curl -L https://fly.io/install.sh | sh
   ```

2. **Login to Fly.io**:

   ```bash
   fly auth login
   ```

3. **Launch your app** (this will use the existing `fly.toml`):

   ```bash
   fly launch
   ```

   - Choose a region close to you (e.g., `ams` for Amsterdam)
   - Don't deploy yet when asked (we'll set secrets first)

4. **Set all environment variables**:

   ```bash
   fly secrets set SLACK_BOT_TOKEN=your-token
   fly secrets set SLACK_SIGNING_SECRET=your-secret
   fly secrets set SPOTIFY_CLIENT_ID=your-id
   fly secrets set SPOTIFY_CLIENT_SECRET=your-secret
   fly secrets set SPOTIFY_REFRESH_TOKEN=your-token
   fly secrets set SPOTIFY_PLAYLIST_ID=your-playlist-id
fly secrets set MUSIC_CHANNEL_NAME=jamcraft
fly secrets set PORT=3000
# Optional: set to "true" for one-time backfill of existing channel messages
# fly secrets set SCAN_EXISTING_ON_STARTUP=true
   ```

5. **Deploy**:

   ```bash
   fly deploy
   ```

6. **Get your app URL**:

   ```bash
   fly status
   ```

   Your app will be at: `https://jamcraft.fly.dev` (or whatever name you chose)

7. **Update external services**:

   - **Slack**: Event Subscriptions → Request URL → `https://jamcraft.fly.dev/slack/events`
   - **Spotify**: Add `https://jamcraft.fly.dev/spotify/callback` to Redirect URIs in your app settings

8. **Check logs**:
   ```bash
   fly logs
   ```

**Note:** ngrok is ONLY for local development. Production uses Fly.io's permanent HTTPS URL.

## Project Structure

```
jamcraft/
├── Cargo.toml
├── README.md
├── .env (create this)
├── src/
│   ├── main.rs          # Axum server and event handling
│   ├── types.rs         # Slack payload structs
│   ├── slack.rs         # Slack API client and signature verification
│   ├── resolve.rs       # URL extraction and Spotify track resolution
│   └── spotify.rs       # Spotify API client with token management
└── scripts/
    └── spotify_auth.rs  # One-time tool to get refresh token
```

## Dependencies

- `axum` - Web framework
- `tokio` - Async runtime
- `reqwest` - HTTP client
- `serde` / `serde_json` - JSON serialization
- `dotenvy` - Environment variable loading
- `tracing` / `tracing-subscriber` - Logging
- `hmac` / `sha2` / `hex` - Slack signature verification
- `dashmap` - Concurrent hash map for deduplication
- `regex` - URL extraction
- `time` - Time utilities

## Troubleshooting

- **"Channel not found"**: Make sure the bot is invited to `#jamcraft` and the channel name matches `MUSIC_CHANNEL_NAME`
- **"Signature verification failed"**: Check that `SLACK_SIGNING_SECRET` is correct
- **"missing_scope" error**: Make sure you have all required scopes (`channels:read`, `channels:history`, `chat:write`, `reactions:write`) and **reinstalled the app** to get a new token with updated permissions
- **"Token refresh failed"**: Verify your Spotify credentials and re-run the auth script if needed
- **No reactions/messages**: Check bot permissions in Slack (OAuth & Permissions) and make sure the bot is invited to the channel
- **Events not received**: Verify the Event Subscriptions URL is correct and accessible via HTTPS. For local dev, make sure ngrok is running and the URL is updated in Slack
- **ngrok requests timing out**: Make sure the bot is running (`cargo run`) and ngrok is forwarding to `127.0.0.1:3000`. Try restarting both.
- **URL verification fails in Slack**: The bot handles this automatically, but if it persists, check that the bot is running and accessible through ngrok

## License

See LICENSE file.
