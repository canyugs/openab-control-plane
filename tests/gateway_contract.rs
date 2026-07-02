//! Gateway wire-protocol contract test (PLAN.md D1).
//!
//! `src/protocol.rs` hand-mirrors the bot-side structs in OpenAB's
//! `crates/openab-core/src/gateway.rs` (the `[gateway]` adapter every stock pod
//! runs). There is no shared crate, so drift is only caught here: the golden
//! fixtures below are derived from openab at the image version pinned in the
//! Zeabur templates.
//!
//! Pinned against: ghcr.io/openabdev/openab:0.9.0-beta.6 (openab commit 22a3cb2).
//! On an image bump, re-check `crates/openab-core/src/gateway.rs` (structs
//! `GatewayEvent`/`GwChannel`/`GwSender`/`GwContent`, `GatewayReply`,
//! `GatewayResponse`) and update the fixtures if fields moved.

use openab_control_plane::protocol::{
    ChannelInfo, Content, GatewayEvent, GatewayReply, GatewayResponse, SenderInfo, EVENT_SCHEMA,
};
use serde_json::{json, Value};

/// Plane → bot: every field the bot's `GatewayEvent` deserializer requires
/// (all fields except `mentions` lack `#[serde(default)]` on the bot side, so
/// a missing/renamed one kills message delivery for every pod at once).
#[test]
fn event_serialization_satisfies_bot_deserializer() {
    let event = GatewayEvent {
        schema: EVENT_SCHEMA.into(),
        event_id: "evt_1".into(),
        timestamp: "2026-07-02T00:00:00Z".into(),
        platform: "feishu".into(),
        event_type: "message".into(),
        channel: ChannelInfo {
            id: "ch_1".into(),
            channel_type: "supergroup".into(),
            thread_id: Some("th_1".into()),
        },
        sender: SenderInfo {
            id: "client".into(),
            name: "client".into(),
            display_name: "client".into(),
            is_bot: false,
        },
        content: Content::text("hello"),
        mentions: vec!["chair".into()],
        message_id: "msg_1".into(),
    };
    let v: Value = serde_json::to_value(&event).unwrap();

    assert_eq!(v["schema"], "openab.gateway.event.v1");
    // Required (no serde default) on the bot side:
    for field in ["event_id", "timestamp", "platform", "channel", "sender", "content", "message_id"] {
        assert!(!v[field].is_null(), "bot requires `{field}`");
    }
    // Nested field names the bot deserializes by exact name:
    assert_eq!(v["channel"]["id"], "ch_1");
    assert_eq!(v["channel"]["type"], "supergroup"); // renamed field — the classic drift spot
    assert_eq!(v["channel"]["thread_id"], "th_1");
    assert_eq!(v["sender"]["id"], "client");
    assert_eq!(v["sender"]["name"], "client");
    assert_eq!(v["sender"]["display_name"], "client");
    assert_eq!(v["sender"]["is_bot"], false);
    assert_eq!(v["content"]["type"], "text");
    assert_eq!(v["content"]["text"], "hello");
    // Empty attachments must be omitted: bot-side `GwAttachment` requires
    // type/filename/mime_type/size per element, and OCP never builds those.
    assert!(v["content"].get("attachments").is_none());
}

/// Bot → plane: fixtures written exactly as openab core serializes them
/// (`GatewayReply` in gateway.rs — plain send, create_topic, add_reaction,
/// edit_message). OCP must accept every one.
#[test]
fn bot_reply_fixtures_deserialize() {
    // Plain in-thread send (send_gateway_reply).
    let plain: GatewayReply = serde_json::from_str(
        r#"{
            "schema": "openab.gateway.reply.v1",
            "reply_to": "evt_1",
            "platform": "feishu",
            "channel": {"id": "ch_1", "thread_id": "th_1"},
            "content": {"type": "text", "text": "findings…"}
        }"#,
    )
    .unwrap();
    assert!(plain.command.is_none());
    assert_eq!(plain.channel.thread_id.as_deref(), Some("th_1"));
    assert_eq!(plain.content.text, "findings…");

    // create_topic carries a request_id and expects a correlated response.
    let topic: GatewayReply = serde_json::from_str(
        r#"{
            "schema": "openab.gateway.reply.v1",
            "reply_to": "",
            "platform": "feishu",
            "channel": {"id": "ch_1"},
            "content": {"type": "text", "text": "Council review"},
            "command": "create_topic",
            "request_id": "req_1"
        }"#,
    )
    .unwrap();
    assert_eq!(topic.command.as_deref(), Some("create_topic"));
    assert_eq!(topic.request_id.as_deref(), Some("req_1"));

    // add_reaction: reply_to is the target message id (done-signal 🆗 path).
    let react: GatewayReply = serde_json::from_str(
        r#"{
            "schema": "openab.gateway.reply.v1",
            "reply_to": "msg_9",
            "platform": "feishu",
            "channel": {"id": "ch_1", "thread_id": "th_1"},
            "content": {"type": "text", "text": "🆗"},
            "command": "add_reaction"
        }"#,
    )
    .unwrap();
    assert_eq!(react.command.as_deref(), Some("add_reaction"));
    assert_eq!(react.reply_to, "msg_9");

    // edit_message (streaming edits) with request_id + quote fallback field absent.
    let edit: GatewayReply = serde_json::from_str(
        r#"{
            "schema": "openab.gateway.reply.v1",
            "reply_to": "msg_9",
            "platform": "feishu",
            "channel": {"id": "ch_1", "thread_id": "th_1"},
            "content": {"type": "text", "text": "updated body"},
            "command": "edit_message",
            "request_id": "req_2"
        }"#,
    )
    .unwrap();
    assert_eq!(edit.command.as_deref(), Some("edit_message"));
    assert!(edit.quote_message_id.is_none());
}

/// Plane → bot ack: the bot's `GatewayResponse` requires `schema`,
/// `request_id`, `success`; `thread_id`/`message_id`/`error` are Options.
#[test]
fn response_shape_matches_bot_expectations() {
    let ok = GatewayResponse::ok("req_1");
    let v: Value = serde_json::to_value(&ok).unwrap();
    assert_eq!(v["schema"], "openab.gateway.response.v1");
    assert_eq!(v["request_id"], "req_1");
    assert_eq!(v["success"], true);
    // Omitted Options serialize as absent; bot maps absent → None.
    for field in ["thread_id", "message_id", "error"] {
        assert!(v.get(field).is_none(), "`{field}` should be omitted when None");
    }

    // A create_topic success carries thread_id; round-trip the enriched form.
    let enriched = json!({
        "schema": "openab.gateway.response.v1",
        "request_id": "req_1",
        "success": true,
        "thread_id": "th_1",
        "message_id": "msg_1"
    });
    let parsed: GatewayResponse = serde_json::from_value(enriched).unwrap();
    assert_eq!(parsed.thread_id.as_deref(), Some("th_1"));
}
