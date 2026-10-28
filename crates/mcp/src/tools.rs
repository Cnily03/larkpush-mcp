use anyhow::{Result, bail};
use larkpush_lark::{LarkClient, ListUsers, SearchUsers, SendMessage, UploadFile, UploadImage};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct ChatArgs {
    #[serde(default)]
    page_size: Option<u8>,
    #[serde(default)]
    page_token: Option<String>,
}

#[derive(Deserialize)]
struct MemberArgs {
    chat_id: String,
    #[serde(default)]
    member_id_type: Option<String>,
    #[serde(default)]
    page_size: Option<u8>,
    #[serde(default)]
    page_token: Option<String>,
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required, "additionalProperties": false}
    })
}

pub(crate) fn list() -> Value {
    let page = json!({"type":"integer","minimum":1,"maximum":100});
    let token = json!({"type":"string"});
    let receiver =
        json!({"type":"string","enum":["open_id","user_id","union_id","email","chat_id"]});
    let content = json!({"type":"object","description":"Lark message content as an object; the server serializes it to the API's JSON string. Examples: {\"text\":\"Hello\"}, {\"image_key\":\"img_...\"}, or a post/card object."});
    let send_props = json!({"receive_id":{"type":"string"},"receive_id_type":receiver,"msg_type":{"type":"string","enum":["text","post","image","file","audio","media","sticker","interactive","share_chat","share_user"]},"content":content,"uuid":{"type":"string"}});
    json!([
        tool(
            "list_users",
            "List app-visible employees and return only open_id values, following up to max_pages pages.",
            json!({"max_pages":{"type":"integer","minimum":1,"maximum":100,"default":20},"page_token":token}),
            &[]
        ),
        tool(
            "search_users",
            "Search app-visible employees by name, email, phone, or employee ID; return only open_id values.",
            json!({"query":{"type":"string","minLength":1},"max_pages":{"type":"integer","minimum":1,"maximum":100,"default":20},"page_token":token}),
            &["query"]
        ),
        tool(
            "list_chats",
            "List groups containing the app bot.",
            json!({"page_size":page,"page_token":token}),
            &[]
        ),
        tool(
            "list_chat_members",
            "List members of a group containing the app bot.",
            json!({"chat_id":{"type":"string"},"member_id_type":{"type":"string","enum":["open_id","user_id","union_id"]},"page_size":page,"page_token":token}),
            &["chat_id"]
        ),
        tool(
            "send_as_app",
            "Send a message as the application bot to a user or chat.",
            send_props,
            &["receive_id", "receive_id_type", "msg_type", "content"]
        ),
        tool(
            "upload_image",
            "Upload a base64 image and return image_key for an image message.",
            json!({"image_base64":{"type":"string"}}),
            &["image_base64"]
        ),
        tool(
            "upload_file",
            "Upload base64 media and return file_key for file, audio, or media messages.",
            json!({"file_base64":{"type":"string"},"file_name":{"type":"string"},"file_type":{"type":"string","enum":["opus","mp4","pdf","doc","xls","ppt","stream"]},"duration":{"type":"integer","minimum":0}}),
            &["file_base64", "file_name", "file_type"]
        )
    ])
}

pub(crate) async fn call(client: &LarkClient, name: &str, args: Value) -> Result<Value> {
    match name {
        "list_users" => {
            client
                .list_users(serde_json::from_value::<ListUsers>(args)?)
                .await
        }
        "search_users" => {
            client
                .search_users(serde_json::from_value::<SearchUsers>(args)?)
                .await
        }
        "list_chats" => {
            let args: ChatArgs = serde_json::from_value(args)?;
            client.list_chats(args.page_size, args.page_token).await
        }
        "list_chat_members" => {
            let args: MemberArgs = serde_json::from_value(args)?;
            client
                .list_chat_members(
                    &args.chat_id,
                    args.member_id_type,
                    args.page_size,
                    args.page_token,
                )
                .await
        }
        "send_as_app" => {
            client
                .send_message(serde_json::from_value::<SendMessage>(args)?)
                .await
        }
        "upload_image" => {
            let args: UploadImage = serde_json::from_value(args)?;
            client.upload_image(args).await
        }
        "upload_file" => {
            let args: UploadFile = serde_json::from_value(args)?;
            client.upload_file(args).await
        }
        _ => bail!("unknown tool: {name}"),
    }
}
