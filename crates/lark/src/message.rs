use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Method, multipart};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::LarkClient;

#[derive(Debug, Deserialize)]
pub struct SendMessage {
    pub receive_id: String,
    pub receive_id_type: String,
    pub msg_type: String,
    pub content: Value,
    #[serde(default)]
    pub uuid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UploadImage {
    pub image_base64: String,
}

#[derive(Debug, Deserialize)]
pub struct UploadFile {
    pub file_base64: String,
    pub file_name: String,
    pub file_type: String,
    #[serde(default)]
    pub duration: Option<u32>,
}

impl LarkClient {
    /// Validate and send an app-bot message to a user or chat.
    pub async fn send_message(&self, args: SendMessage) -> Result<Value> {
        let (receive_id_type, body) = message_request(args)?;
        self.request(
            Method::POST,
            "im/v1/messages",
            &[("receive_id_type", receive_id_type)],
            Some(body),
        )
        .await
    }

    /// Decode and upload a base64 image within Lark's size limit.
    pub async fn upload_image(&self, args: UploadImage) -> Result<Value> {
        let bytes = STANDARD
            .decode(&args.image_base64)
            .context("invalid image_base64")?;
        if bytes.is_empty() || bytes.len() > 10 * 1024 * 1024 {
            bail!("image must be between 1 byte and 10 MiB");
        }
        let form = multipart::Form::new().text("image_type", "message").part(
            "image",
            multipart::Part::bytes(bytes).file_name("image.png"),
        );
        self.upload("im/v1/images", form).await
    }

    /// Validate and upload base64 file or media content.
    pub async fn upload_file(&self, args: UploadFile) -> Result<Value> {
        let bytes = STANDARD
            .decode(&args.file_base64)
            .context("invalid file_base64")?;
        if bytes.is_empty() || bytes.len() > 30 * 1024 * 1024 {
            bail!("file must be between 1 byte and 30 MiB");
        }
        if args.file_name.trim().is_empty() {
            bail!("file_name is required");
        }
        if !matches!(
            args.file_type.as_str(),
            "opus" | "mp4" | "pdf" | "doc" | "xls" | "ppt" | "stream"
        ) {
            bail!("unsupported file_type");
        }
        let mut form = multipart::Form::new()
            .text("file_type", args.file_type)
            .text("file_name", args.file_name.clone())
            .part(
                "file",
                multipart::Part::bytes(bytes).file_name(args.file_name),
            );
        if let Some(duration) = args.duration {
            form = form.text("duration", duration.to_string());
        }
        self.upload("im/v1/files", form).await
    }
}

/// Lark requires `content` to be a JSON-encoded string inside the request object.
fn message_request(args: SendMessage) -> Result<(String, Value)> {
    if args.receive_id.trim().is_empty() {
        bail!("receive_id is required");
    }
    if !matches!(
        args.receive_id_type.as_str(),
        "open_id" | "user_id" | "union_id" | "email" | "chat_id"
    ) {
        bail!("receive_id_type must be open_id, user_id, union_id, email, or chat_id");
    }
    if !matches!(
        args.msg_type.as_str(),
        "text"
            | "post"
            | "image"
            | "file"
            | "audio"
            | "media"
            | "sticker"
            | "interactive"
            | "share_chat"
            | "share_user"
    ) {
        bail!("unsupported msg_type");
    }
    if !args.content.is_object() {
        bail!("content must be a JSON object");
    }
    let content = serde_json::to_string(&args.content)?;
    let mut body =
        json!({"receive_id": args.receive_id, "msg_type": args.msg_type, "content": content});
    if let Some(uuid) = args.uuid {
        body["uuid"] = json!(uuid);
    }
    Ok((args.receive_id_type, body))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SendMessage, message_request};

    #[test]
    fn serializes_message_content_for_lark() {
        let (receiver_type, body) = message_request(SendMessage {
            receive_id: "oc_chat".into(),
            receive_id_type: "chat_id".into(),
            msg_type: "text".into(),
            content: json!({"text":"Hello"}),
            uuid: None,
        })
        .unwrap();
        assert_eq!(receiver_type, "chat_id");
        assert_eq!(body["content"], "{\"text\":\"Hello\"}");
    }
}
