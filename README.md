# Larkpush

_**English** | [简体中文](README.zh-CN.md)_

`larkpush` is an OAuth-protected MCP server for finding Lark Office users and chats and sending messages as a Lark Office application bot.

## 📄 Contents

- [Get started](#get-started)
  - [Create a Lark Office app](#create-a-lark-office-app)
  - [Switch API URLs](#switch-api-urls)
  - [Tools and permissions](#tools-and-permissions)
- [Deployment](#deployment)
  - [CLI](#cli)
  - [Cloudflare Worker](#cloudflare-worker)
- [Send a message](#send-a-message)
- [Use in ChatGPT](#use-in-chatgpt)
- [License](#license)

## 💡 Get started

### Create a Lark Office app

1. Go to [Lark Office Open Platform](https://open.larksuite.com).
2. Create a Lark Office custom app and enable its bot capability.
3. Grant the application identity permissions [needed for the MCP tools](#tools-and-permissions).
4. Publish the app version. Keep the app ID and secret for deployment.

### Switch API URLs

`config.toml` and `wrangler.toml` can specify OpenAPI endpoints. It uses Lark Office endpoints by default and gives both alternatives. You can switch URLs by commenting/uncommenting them. For [setting up by Cloudflare Worker button](#deploy-with-the-button), please modify the environment variables manually when setting up or in Worker settings.

### Tools and permissions

Enable the following **application identity** scopes in the Lark Office developer console. The directory tools return Open IDs only and request no department or profile fields.

| MCP tool | Purpose | Lark Office scope | Lark Office API |
| --- | --- | --- | --- |
| `list_users` | List app-visible employees and their Open IDs. | `directory:employee:list` | [Directory API Explorer](https://open.larksuite.com/document/tools-and-resources/api-explorer-guide) |
| `search_users` | Search app-visible employees and return Open IDs. | `directory:employee:search` | [Directory API Explorer](https://open.larksuite.com/document/tools-and-resources/api-explorer-guide) |
| `list_chats` | List groups containing the bot. | `im:chat:read` or `im:chat:readonly` | [List chats](https://open.larksuite.com/document/server-docs/group/chat/list) |
| `list_chat_members` | List members of a group containing the bot. | `im:chat.members:read` or `im:chat:readonly` | [List members](https://open.larksuite.com/document/server-docs/group/chat-member/get) |
| `send_as_app` | Send text, rich text, images, cards, or files as the bot. | `im:message:send_as_bot` or `im:message` | [Send message](https://open.larksuite.com/document/server-docs/im-v1/message/create) |
| `send_multi_users` | Queue the same message for multiple users. | `im:message:send_multi_users` and `im:message:send_as_bot` (or `im:message`) | [Send messages in batches](https://open.larksuite.com/document/server-docs/im-v1/batch_message/send-messages-in-batches) |
| `upload_image` | Upload an image and return an `image_key`. | `im:resource` | [Upload image](https://open.larksuite.com/document/server-docs/im-v1/image/create) |
| `upload_file` | Upload a file and return a `file_key`. | `im:resource` | [Upload file](https://open.larksuite.com/document/server-docs/im-v1/file/create) |

`list_users` and `search_users` use Lark Office's Directory APIs and return `items` containing `open_id`, plus `has_more` and `next_page_token` for pagination. Names require an additional Directory field scope such as `directory:employee.base.name.name:read`, so they are not requested with the current permissions. The bot must belong to a target group, and a direct-message recipient must be within the app's availability range. The optional `[lark].lark_scope` applies to the user's OAuth login, separately from these application scopes.

## 🚀 Deployment

Choose either the native CLI server or the Cloudflare Worker.

### ⚙️ CLI

Install Rust and Cargo:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Clone the repository:

```bash
git clone https://github.com/Cnily03/larkpush-mcp.git
cd larkpush-mcp
```

Build or install the CLI, this would build the binart `larkpush` for CLI and `migrate-db` for [Cloudflare Worker migration](#migrate-to-cloudflare-worker):

```bash
# build the CLI
cargo build --release
# or install it to $HOME/.cargo/bin
cargo install --path .
```

Configuration:

1. Copy `config.example.toml` to `config.toml`.
2. Set the **App ID** and **App Secret** in `[lark]`. \
   *Ref: https://open.larksuite.com/app/{APP_ID}/baseinfo*
3. Set `[server].public_base_url` to the public HTTPS origin (for example, `https://your-domain.com`).
4. Add `https://your-domain/oauth/feishu/callback` to the app's Lark Office OAuth redirect URLs on [Lark Office Open Platform](https://open.larksuite.com), using the same origin as `public_base_url`. \
   *Ref: https://open.larksuite.com/app/{APP_ID}/safe*

The MCP URL would be `https://your-domain/mcp` according to examples above.

> [!Caution]
> Please keep `config.toml` private. DO NOT commit it to a public repository.
>

To run the native Rust server:

```bash
target/release/larkpush mcp -L -c config.toml # if build
larkpush mcp -L -c config.toml                # if install
```

`-L` uses `[server].listen` by default; `-L=0.0.0.0:3000` overrides it. It's recommended to expose the server through HTTPS and forward all its routes through the reverse proxy.

#### Database storage

The native server stores persistent data like OAuth clients and token grants by SQLite in `.data/larkpush.sqlite` beside the config file. Set `[server].database_path` to use another location.

State storage like pending logins and authorization codes use an *in-memory key-value cache* with short expiries by a background cleanup worker. That means restarting the server will require unfinished logins to start over. And the server does NOT support multiple instances behind a load balancer or cluster.

#### Token lifetime and rotation

MCP access tokens last one hour. The `/token` endpoint supports the `refresh_token` grant, rotating refresh tokens for up to 90 days of inactivity, so an OAuth client can renew access without another Lark Office login. Existing connections created before this feature need one new login to receive a refresh token.

### ⛅ Cloudflare Worker

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/Cnily03/larkpush-mcp)

This repository also provides a serverless implementation as a Cloudflare Worker.

The worker uses [Cloudflare D1](https://developers.cloudflare.com/d1/) and [Upstash Redis](https://developers.cloudflare.com/workers/databases/third-party-integrations/upstash/). You need to configure them before deploying the worker.

#### Deploy with the button

Please [create an Upstash database](https://docs.upstash.com/redis#create-a-database) first.

The button imports the repository root, including the shared crates, and provisions the D1 database. The Worker initializes its schema on the first request.

- Create or select the D1 Database.
- According to different development platforms and service regions, if you want to switch OpenAPI endpoints, set or modify OpenAPI endpoints variables manually.
- Set `PUBLIC_BASE_URL` to the Worker's final HTTPS origin.
- Set `LARK_APP_ID` to the Lark Office **App ID**.
- Set `LARK_APP_SECRET` to the Lark Office **App Secret** as a secret.
- Set `REDIS_URL` to the Upstash Redis URL as a secret, with the format `rediss://default:password@host:6379` from the [Upstash Console](https://console.upstash.com).

Before connecting a client, do not forget to register `{PUBLIC_BASE_URL}/oauth/feishu/callback` as a redirect URL on [Lark Office Open Platform](https://open.larksuite.com). If the final URL is assigned after deployment, update `PUBLIC_BASE_URL` and redeploy it.

#### Manual setup

Install Wrangler, the Rust WebAssembly target, and the Rust build tool.

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

It's recommended to create a local Wrangler configuration and `.env`:

```bash
cp wrangler.toml wrangler.local.toml
cp .env.example .env
```

In `wrangler.local.toml`, set `database_id` to the returned database UUID.

In `[vars]`:

- Switch OpenAPI endpoints by commenting/uncommenting the URLs according to your needs.
- Set `PUBLIC_BASE_URL` as your worker's final HTTPS origin.
- Set `LARK_APP_ID` to Lark Office **App ID**.

Store the Lark Office **App Secret** and Upstash Redis URL in `.env` and upload them to Cloudflare, then deploy:

```bash
wrangler -c wrangler.local.toml secret put LARK_APP_SECRET
wrangler -c wrangler.local.toml secret put REDIS_URL
wrangler -c wrangler.local.toml deploy
```

Register `{PUBLIC_BASE_URL}/oauth/feishu/callback` as a redirect URL on [Lark Office Open Platform](https://open.larksuite.com).

#### Local development

Set `PUBLIC_BASE_URL` to local origin like `http://localhost:8787`, then register the corresponding redirect URL (*e.g. `http://localhost:8787/oauth/feishu/callback`*) on [Lark Office Open Platform](https://open.larksuite.com), then run:

```bash
wrangler -c wrangler.local.toml dev --port 8787 # --var PUBLIC_BASE_URL:http://localhost:8787
```

> [!Note]
> The root `.env`, `wrangler.local.toml`, and build output are ignored by Git. `.env` only supplies local secrets.
>
> Please use the secrets set by `wrangler secret put` for deployed workers.
>
> Local development uses the configured Upstash Redis database, so use a **separate** database from production.
>

Worker logs show UTC time, level, scope, and request details in the `wrangler dev` terminal. To view logs from a deployed Worker:

```bash
wrangler -c wrangler.local.toml tail --format pretty
```

#### Migrate to Cloudflare Worker

The repository provides a migration tool to copy the SQLite database from the native server (CLI) to Cloudflare D1 or local worker development environment.

Stop the native server before copying its SQLite database file.

Pass the SQLite database file directly to `migrate-db`, it would execute D1 initialization and migration.

```bash
# Migrate to local D1 for development
cargo run --bin migrate-db -- -c wrangler.local.toml .data/larkpush.sqlite --local
# Migrate to remote D1 for production
cargo run --bin migrate-db -- -c wrangler.local.toml .data/larkpush.sqlite --remote
```

The configuration defaults to `wrangler.toml` for either destination; use `-c` to select another Wrangler file.

Note that the migration tool won't replace any data value ​​due to configuration differences. It only copies the data.

#### Database storage

The D1 Database uses same schema as SQLite in the native server.

The Worker initializes the D1 schema automatically on first use. [Migrating](#migrate-to-cloudflare-worker) also initializes it before importing data.

State storages use external Redis database with short expiries, and would be consumed or cleaned up automatically.

Local D1 data is stored under `.wrangler/state` by default and is separate from the remote database, while the redis is not. You should use a separate Redis database for local development to avoid conflicts with production data.

As it's a serverless application, the Worker supports multiple instances or cluster deployment.

## 💬 Send a message

`send_as_app` takes `receive_id`, `receive_id_type`, `msg_type`, and a JSON object in `content`. For example:

```json
{"receive_id":"ou_...","receive_id_type":"open_id","msg_type":"text","content":{"text":"Hello"}}
```

For a group, use `receive_id_type: "chat_id"` and the group's `chat_id`. To send an image or file, call `upload_image` or `upload_file` first, then send the returned key. See [Lark Office message content formats](https://open.larksuite.com/document/server-docs/im-v1/message-content-description/create_json) for rich text and other message types.

`send_multi_users` queues the same message for multiple users. It accepts Open IDs from `list_users` or `search_users` and returns a batch message ID plus any invalid Open IDs:

```json
{"open_ids":["ou_...","ou_..."],"msg_type":"text","content":{"text":"Hello"}}
```

For `msg_type: "interactive"`, pass the card as `content.card`. Batch messages are asynchronous, cannot be sent to groups, and do not bypass the bot's availability range.

## 🤖 Use in ChatGPT

In [ChatGPT Plugins](https://chatgpt.com/plugins), add the MCP URL with OAuth. The MCP URL is like `https://your-domain/mcp` according to examples above.

Once the MCP URL is typed in, ChatGPT would automatically discover the OAuth configurations. If not, please check whether the `/.well-known/oauth*` endpoint is forbidden by your reverse proxy or firewall.

Create and install the plugin, try to login to the plugin by Lark Office account. Then start a new conversation and ask ChatGPT to send a message to a Lark Office user or group.

For more details, please refer to the official documentation: [Connect and test your plugin](https://developers.openai.com/plugins/deploy/connect-chatgpt).

## 🎫 License

Copyright (c) Cnily03. All rights reserved.

Licensed under the [MIT License](LICENSE).
