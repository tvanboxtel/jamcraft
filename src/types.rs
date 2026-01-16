use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SlackEnvelope {
    #[serde(rename = "type")]
    pub event_type: String,
    pub challenge: Option<String>,
    pub event: Option<SlackEvent>,
}

#[derive(Debug, Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub event_type: String,
    pub text: Option<String>,
    pub channel: Option<String>,
    pub ts: Option<String>,
    pub bot_id: Option<String>,
    pub subtype: Option<String>,
}
