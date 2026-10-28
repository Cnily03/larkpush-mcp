# larkpush

`larkpush` is an OAuth-protected MCP server for finding Lark users and chats and sending messages as a Lark application bot.

## Get started

1. Create a Lark custom app, enable its bot capability, grant the [app permissions](#tools-and-permissions) you need, and publish the app version.
2. Copy `config.example.toml` to `config.toml`. Set the app ID and secret in `[lark]`, then set `[server].public_base_url` to the public HTTPS origin. Keep `config.toml` private.
3. Add `https://your-domain/oauth/feishu/callback` to the app's Lark OAuth redirect URLs, using the same origin as `public_base_url`.
4. Build and run the server:

```sh
cargo build --release
target/release/larkpush mcp -L -c config.toml
```

`-L` uses `[server].listen`; `-L=0.0.0.0:3000` overrides it. Expose the server through HTTPS and forward all its routes through the reverse proxy. The MCP URL is `https://your-domain/mcp`.

In [ChatGPT Plugins](https://developers.openai.com/plugins/deploy/connect-chatgpt), add that MCP URL with OAuth, create and install the plugin, then start a new conversation and select it with `@`. The server stores registered OAuth clients and hashed access and refresh token grants in `.data/` beside the config file; keep this directory across restarts. MCP access tokens last one hour. The `/token` endpoint also supports the `refresh_token` grant, rotating refresh tokens for up to 90 days of inactivity, so an OAuth client can renew access without another Lark login. Existing connections created before this feature need one new login to receive a refresh token.

## Tools and permissions

Enable the following **application identity** scopes in the Lark developer console. The directory tools return Open IDs only and request no department or profile fields.

| MCP tool | Purpose | Lark scope | Lark API |
| --- | --- | --- | --- |
| `list_users` | List app-visible employees and their Open IDs. | `directory:employee:list` | [Filter employees](https://open.larkoffice.com/document/uAjLw4CM/ukTMukTMukTM/directory-v1/employee/filter) |
| `search_users` | Search app-visible employees and return Open IDs. | `directory:employee:search` | [Search employees](https://open.larkoffice.com/document/uAjLw4CM/ukTMukTMukTM/directory-v1/employee/search) |
| `list_chats` | List groups containing the bot. | `im:chat:read` or `im:chat:readonly` | [List chats](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat/list) |
| `list_chat_members` | List members of a group containing the bot. | `im:chat.members:read` or `im:chat:readonly` | [List members](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat-members/get) |
| `send_as_app` | Send text, rich text, images, cards, or files as the bot. | `im:message:send_as_bot` or `im:message` | [Send message](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message/create) |
| `upload_image` | Upload an image and return an `image_key`. | `im:resource` | [Upload image](https://open.feishu.cn/document/server-docs/im-v1/image/create) |
| `upload_file` | Upload a file and return a `file_key`. | `im:resource` | [Upload file](https://open.feishu.cn/document/server-docs/im-v1/file/create) |

`list_users` and `search_users` use Lark's Directory APIs and return `items` containing `open_id`, plus `has_more` and `next_page_token` for pagination. Names require an additional Directory field scope such as `directory:employee.base.name.name:read`, so they are not requested with the current permissions. The bot must belong to a target group, and a direct-message recipient must be within the app's availability range. The optional `[lark].lark_scope` applies to the user's OAuth login, separately from these application scopes.

## Send a message

`send_as_app` takes `receive_id`, `receive_id_type`, `msg_type`, and a JSON object in `content`. For example:

```json
{"receive_id":"ou_...","receive_id_type":"open_id","msg_type":"text","content":{"text":"Hello"}}
```

For a group, use `receive_id_type: "chat_id"` and the group's `chat_id`. To send an image or file, call `upload_image` or `upload_file` first, then send the returned key. See [Lark message content formats](https://open.feishu.cn/document/server-docs/im-v1/message-content-description/create_json) for rich text and other message types.

## License

Licensed under the [MIT License](LICENSE).
