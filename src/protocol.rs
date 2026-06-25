//! Gateway wire protocol (design §2/§11), mirroring OpenAB's
//! crates/openab-gateway schema so a stock bot's `[gateway]` adapter speaks to
//! us unchanged. Plane→bot: GatewayEvent. Bot→plane: GatewayReply. Plane→bot:
//! GatewayResponse (correlated by request_id).

use serde::{Deserialize, Serialize};

pub const EVENT_SCHEMA: &str = "openab.gateway.event.v1";
pub const RESPONSE_SCHEMA: &str = "openab.gateway.response.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub channel_type: String, // "dm" | "group" | "supergroup"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderInfo {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub is_bot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    #[serde(rename = "type", default = "text_type")]
    pub content_type: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<serde_json::Value>,
}

fn text_type() -> String {
    "text".into()
}

impl Content {
    pub fn text(s: impl Into<String>) -> Content {
        Content { content_type: "text".into(), text: s.into(), attachments: vec![] }
    }
}

/// Plane → bot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayEvent {
    pub schema: String,
    pub event_id: String,
    pub timestamp: String,
    pub platform: String,
    pub event_type: String,
    pub channel: ChannelInfo,
    pub sender: SenderInfo,
    pub content: Content,
    #[serde(default)]
    pub mentions: Vec<String>,
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyChannel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

/// Bot → plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayReply {
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub reply_to: String,
    #[serde(default)]
    pub platform: String,
    pub channel: ReplyChannel,
    #[serde(default = "default_content")]
    pub content: Content,
    /// None = plain send. Else: create_topic | add_reaction | remove_reaction |
    /// edit_message | delete_message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_message_id: Option<String>,
}

fn default_content() -> Content {
    Content::text("")
}

/// Plane → bot, ack of a reply that carried a request_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayResponse {
    pub schema: String,
    pub request_id: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl GatewayResponse {
    pub fn ok(request_id: &str) -> GatewayResponse {
        GatewayResponse {
            schema: RESPONSE_SCHEMA.into(),
            request_id: request_id.into(),
            success: true,
            thread_id: None,
            message_id: None,
            error: None,
        }
    }
}
