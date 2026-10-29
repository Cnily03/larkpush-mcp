# Larkpush

`larkpush` is an OAuth-protected MCP server for finding Lark users and chats and sending messages as a Lark application bot.

## Contents

- [Get started](#get-started)
  - [Create a Lark app](#create-a-lark-app)
  - [Tools and permissions](#tools-and-permissions)
- [Deployment](#deployment)
  - [CLI](#cli)
  - [Cloudflare Worker](#cloudflare-worker)
- [Send a message](#send-a-message)
- [Use in ChatGPT](#use-in-chatgpt)
- [License](#license)

## Get started

### Create a Lark app

Create a Lark custom app and enable its bot capability. Grant the application identity permissions needed for the MCP tools you plan to use, then publish the app version. Keep the app ID and secret for deployment.

### Tools and permissions

Enable the following **application identity** scopes in the Lark developer console. The directory tools return Open IDs only and request no department or profile fields.

| MCP tool | Purpose | Lark scope | Lark API |
| --- | --- | --- | --- |
| `list_users` | List app-visible employees and their Open IDs. | `directory:employee:list` | [Filter employees](https://open.feishu.cn/document/directory-v1/employee/filter) |
| `search_users` | Search app-visible employees and return Open IDs. | `directory:employee:search` | [Search employees](https://open.feishu.cn/document/directory-v1/employee/search) |
| `list_chats` | List groups containing the bot. | `im:chat:read` or `im:chat:readonly` | [List chats](https://open.feishu.cn/document/server-docs/group/chat/list) |
| `list_chat_members` | List members of a group containing the bot. | `im:chat.members:read` or `im:chat:readonly` | [List members](https://open.feishu.cn/document/server-docs/group/chat-member/get) |
| `send_as_app` | Send text, rich text, images, cards, or files as the bot. | `im:message:send_as_bot` or `im:message` | [Send message](https://open.feishu.cn/document/server-docs/im-v1/message/create) |
| `send_multi_users` | Queue the same message for multiple users. | `im:message:send_multi_users` and `im:message:send_as_bot` (or `im:message`) | [Send messages in batches](https://open.feishu.cn/document/server-docs/im-v1/batch_message/send-messages-in-batches) |
| `upload_image` | Upload an image and return an `image_key`. | `im:resource` | [Upload image](https://open.feishu.cn/document/server-docs/im-v1/image/create) |
| `upload_file` | Upload a file and return a `file_key`. | `im:resource` | [Upload file](https://open.feishu.cn/document/server-docs/im-v1/file/create) |

`list_users` and `search_users` use Lark's Directory APIs and return `items` containing `open_id`, plus `has_more` and `next_page_token` for pagination. Names require an additional Directory field scope such as `directory:employee.base.name.name:read`, so they are not requested with the current permissions. The bot must belong to a target group, and a direct-message recipient must be within the app's availability range. The optional `[lark].lark_scope` applies to the user's OAuth login, separately from these application scopes.

## Deployment

Choose either the native CLI server or the Cloudflare Worker.

### CLI

To run the native Rust server:

1. Copy `config.example.toml` to `config.toml`. Set the app ID and secret in `[lark]`, then set `[server].public_base_url` to the public HTTPS origin. Keep `config.toml` private.
2. Add `https://your-domain/oauth/feishu/callback` to the app's Lark OAuth redirect URLs, using the same origin as `public_base_url`.
3. Build and run the server:

   ```bash
   cargo build --release
   target/release/larkpush mcp -L -c config.toml
   ```

`-L` uses `[server].listen`; `-L=0.0.0.0:3000` overrides it. Expose the server through HTTPS and forward all its routes through the reverse proxy. The MCP URL is `https://your-domain/mcp`.

#### OAuth tokens and persistence

The native server stores OAuth clients and token grants in `.data/larkpush.sqlite` beside the config file. Pending logins and authorization codes use an in-memory key-value cache and expire after five minutes; a background worker removes expired entries every minute. An unfinished login must be restarted after a server restart. Keep the SQLite database across restarts. Set `[server].database_path` to use another location. The current schema does not import older JSON files or upgrade earlier SQLite schemas; use a fresh database when updating from those versions.

MCP access tokens last one hour. The `/token` endpoint supports the `refresh_token` grant, rotating refresh tokens for up to 90 days of inactivity, so an OAuth client can renew access without another Lark login. Existing connections created before this feature need one new login to receive a refresh token.

### Cloudflare Worker

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/Cnily03/larkpush-mcp)

The worker uses [Cloudflare D1](https://developers.cloudflare.com/d1/) and [Upstash Redis](https://developers.cloudflare.com/workers/databases/third-party-integrations/upstash/). You need to configure them before deploying the worker.

#### Deploy with the button

The button imports the repository root, including the shared crates, and provisions the D1 database. The Worker initializes its schema on the first request.

Set `PUBLIC_BASE_URL` to the Worker's final HTTPS origin, `LARK_APP_ID` to the Lark app ID, and `LARK_APP_SECRET` and `REDIS_URL` as secrets.

Use the TLS connection string from the Upstash Console for `REDIS_URL` (`rediss://default:password@host:6379`).

If the final URL is assigned after deployment, update `PUBLIC_BASE_URL` and redeploy before connecting a client. Register `PUBLIC_BASE_URL/oauth/feishu/callback` in the Lark/Feishu app.

#### Manual setup

Install Wrangler, the Rust WebAssembly target, and the Rust build tool. Ensure `worker-build` is on your `PATH`.

```bash
npm install -g wrangler
rustup target add wasm32-unknown-unknown
cargo install worker-build@^0.8
```

From the repository root, authenticate and create a D1 database:

```bash
wrangler login
wrangler d1 create larkpush
```

Set the returned database UUID, `PUBLIC_BASE_URL`, and `LARK_APP_ID` in the root `wrangler.toml`. Store the Lark/Feishu app secret and Upstash TLS Redis URL, then deploy:

```bash
wrangler secret put LARK_APP_SECRET
wrangler secret put REDIS_URL
wrangler deploy
```

Register `PUBLIC_BASE_URL/oauth/feishu/callback` in the Lark/Feishu app. The Worker D1 database is separate from the native server's SQLite database; existing OAuth connections must authorize once against the Worker URL unless migrated.

#### Local development

From the repository root, create a local Wrangler configuration and `.env`:

```bash
cp wrangler.toml wrangler.local.toml
cp .env.example .env
```

Set `LARK_APP_ID` in `wrangler.local.toml`, `LARK_APP_SECRET` and `REDIS_URL` in `.env`. Register `http://127.0.0.1:8787/oauth/feishu/callback` in Lark/Feishu, then run:

```bash
wrangler dev -c wrangler.local.toml --port 8787 # --var PUBLIC_BASE_URL:http://127.0.0.1:8787
```

The root `.env`, `wrangler.local.toml`, and build output are ignored by Git. `.env` supplies local secrets; deployed Workers use the secrets set by `wrangler secret put`. Local development uses the configured Upstash Redis database, so use a separate database from production.

Worker logs show UTC time, level, scope, and request details in the `wrangler dev` terminal. To view logs from a deployed Worker:

```bash
wrangler tail -c wrangler.toml --format pretty
```

#### Migrate to Cloudflare Worker

Stop the native server before copying its SQLite database file. Pass that file directly to `migrate-db`; it applies the D1 schema and imports registered OAuth clients and token pairs that are still valid. It does not migrate pending logins or authorization codes from cache, or remove the source file. The import preserves each token's MCP resource. If the Worker uses a different MCP URL, clients must sign in again to obtain tokens for that URL.

```bash
cargo run --bin migrate-db -- .data/larkpush.sqlite --local -c wrangler.local.toml
cargo run --bin migrate-db -- .data/larkpush.sqlite --remote
```

The configuration defaults to `wrangler.toml` for either destination; use `-c` to select another Wrangler file.

The local command uses local D1 state. The remote command above uses the D1 database UUID in wrangler configuration.

#### Database storage

SQLite and D1 use two tables: clients (including their redirect URLs) and token pairs. Each token pair has an integer `id` primary key; rotating a refresh token replaces both tokens in that row. The revised schema drops the old split token tables on startup, so clients using those tokens must sign in again. OAuth discovery metadata comes from `PUBLIC_BASE_URL` or `[server].public_base_url`; it is not stored in the database. Tokens are stored directly in the table, so protect database access and backups. Worker login states and authorization codes have five-minute Redis expiries and are consumed with `GETDEL`. The Worker initializes the D1 schema automatically on first use; `migrate-db` also initializes it before importing data. Local D1 data is stored under `.wrangler/state` by default and is separate from the remote database.

## Send a message

`send_as_app` takes `receive_id`, `receive_id_type`, `msg_type`, and a JSON object in `content`. For example:

```json
{"receive_id":"ou_...","receive_id_type":"open_id","msg_type":"text","content":{"text":"Hello"}}
```

For a group, use `receive_id_type: "chat_id"` and the group's `chat_id`. To send an image or file, call `upload_image` or `upload_file` first, then send the returned key. See [Lark message content formats](https://open.feishu.cn/document/server-docs/im-v1/message-content-description/create_json) for rich text and other message types.

`send_multi_users` queues the same message for multiple users. It accepts Open IDs from `list_users` or `search_users` and returns a batch message ID plus any invalid Open IDs:

```json
{"open_ids":["ou_...","ou_..."],"msg_type":"text","content":{"text":"Hello"}}
```

For `msg_type: "interactive"`, pass the card as `content.card`. Batch messages are asynchronous, cannot be sent to groups, and do not bypass the bot's availability range.

## Use in ChatGPT

In [ChatGPT Plugins](https://developers.openai.com/plugins/deploy/connect-chatgpt), add the MCP URL with OAuth, create and install the plugin, then start a new conversation and select it with `@`.

## License

Copyright (c) Cnily03. All rights reserved.

Licensed under the [MIT License](LICENSE).
