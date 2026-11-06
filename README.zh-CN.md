# Larkpush

_[English](README.md) | **简体中文**_

`larkpush` 是一个受 OAuth 保护的 MCP 服务器，用于查找飞书用户和群聊，并以飞书应用机器人的身份发送消息。

<a name="contents"></a>
## 📄 目录

- [开始使用](#get-started)
  - [创建飞书应用](#create-a-lark-office-app)
  - [切换 API 地址](#switch-api-urls)
  - [工具和权限](#tools-and-permissions)
- [部署](#deployment)
  - [CLI](#cli)
  - [Cloudflare Worker](#cloudflare-worker)
- [发送消息](#send-a-message)
- [在 ChatGPT 中使用](#use-in-chatgpt)
- [License](#license)

<a name="get-started"></a>
## 💡 开始使用

<a name="create-a-lark-office-app"></a>
### 创建飞书应用

1. 前往[飞书开放平台](https://open.feishu.cn)。
2. 创建飞书自建应用并启用机器人能力。
3. 为应用身份授予 [MCP 工具所需的权限](#tools-and-permissions)。
4. 发布应用版本。保存 App ID 和 App Secret，以便部署时使用。

<a name="switch-api-urls"></a>
### 切换 API 地址

`config.toml` 和 `wrangler.toml` 可以指定 OpenAPI 端点。默认使用海外版地址，并提供两组可选地址。可以通过注释或取消注释相应条目来切换 URL。使用 [Cloudflare Worker 部署按钮](#deploy-with-the-button)时，请在配置部署时或 Worker 设置中手动修改环境变量。

<a name="tools-and-permissions"></a>
### 工具和权限

在飞书开发者后台启用以下**应用身份**权限。通讯录工具仅返回 Open ID，不请求部门或个人资料字段。

| MCP 工具 | 用途 | 飞书权限 | 飞书 API |
| --- | --- | --- | --- |
| `list_users` | 列出应用可见的员工及其 Open ID。 | `directory:employee:list` | [批量获取员工列表](https://open.feishu.cn/document/directory-v1/employee/filter) |
| `search_users` | 搜索应用可见的员工并返回 Open ID。 | `directory:employee:search` | [搜索员工](https://open.feishu.cn/document/directory-v1/employee/search) |
| `list_chats` | 列出机器人所在的群聊。 | `im:chat:read` 或 `im:chat:readonly` | [获取群列表](https://open.feishu.cn/document/server-docs/group/chat/list) |
| `list_chat_members` | 列出机器人所在群聊的成员。 | `im:chat.members:read` 或 `im:chat:readonly` | [获取群成员列表](https://open.feishu.cn/document/server-docs/group/chat-member/get) |
| `send_as_app` | 以机器人身份发送文本、富文本、图片、卡片或文件。 | `im:message:send_as_bot` 或 `im:message` | [发送消息](https://open.feishu.cn/document/server-docs/im-v1/message/create) |
| `send_multi_users` | 向多个用户排队发送相同的消息。 | `im:message:send_multi_users` 和 `im:message:send_as_bot`（或 `im:message`） | [批量发送消息](https://open.feishu.cn/document/server-docs/im-v1/batch_message/send-messages-in-batches) |
| `upload_image` | 上传图片并返回 `image_key`。 | `im:resource` | [上传图片](https://open.feishu.cn/document/server-docs/im-v1/image/create) |
| `upload_file` | 上传文件并返回 `file_key`。 | `im:resource` | [上传文件](https://open.feishu.cn/document/server-docs/im-v1/file/create) |

`list_users` 和 `search_users` 使用飞书的 Directory API，返回的 `items` 包含 `open_id`，并通过 `has_more` 和 `next_page_token` 支持分页。姓名需要额外的 Directory 字段权限，例如 `directory:employee.base.name.name:read`，因此当前权限下不会请求姓名。机器人必须在目标群聊中，而单聊接收者必须在应用可用范围内。可选的 `[lark].lark_scope` 用于用户 OAuth 登录，与上述应用权限分开。

<a name="deployment"></a>
## 🚀 部署

选择原生 CLI 服务器或 Cloudflare Worker。

<a name="cli"></a>
### ⚙️ CLI

安装 Rust 和 Cargo：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

克隆仓库：

```bash
git clone https://github.com/Cnily03/larkpush-mcp.git
cd larkpush-mcp
```

构建或安装 CLI；此操作会构建用于 CLI 的 `larkpush` 二进制文件，以及用于 [Cloudflare Worker 迁移](#migrate-to-cloudflare-worker)的 `migrate-db`：

```bash
# 构建 CLI
cargo build --release
# 或安装到 $HOME/.cargo/bin
cargo install --path .
```

配置：

1. 将 `config.example.toml` 复制为 `config.toml`。
2. 在 `[lark]` 中设置 **App ID** 和 **App Secret**。 \
   *Ref: https://open.feishu.cn/app/{APP_ID}/baseinfo*
3. 将 `[server].public_base_url` 设置为公网 HTTPS 来源地址（例如 `https://your-domain.com`）。
4. 在[飞书开放平台](https://open.feishu.cn)为应用添加 `https://your-domain/oauth/feishu/callback` 作为 OAuth 重定向 URL，其来源地址必须与 `public_base_url` 相同。 \
   *Ref: https://open.feishu.cn/app/{APP_ID}/safe*

按照上面的示例，MCP URL 为 `https://your-domain/mcp`。

> [!Caution]
> 请妥善保管 `config.toml`。不要将其提交到公开仓库。
>

运行原生 Rust 服务器：

```bash
target/release/larkpush mcp -L -c config.toml # 已构建
larkpush mcp -L -c config.toml                # 已安装
```

`-L` 默认使用 `[server].listen`；`-L=0.0.0.0:3000` 可以覆盖它。建议通过 HTTPS 对外提供服务，并让反向代理转发所有路由。

<a name="database-storage"></a>
#### 数据库存储

原生服务器使用 SQLite 将 OAuth 客户端和令牌授权等持久化数据存储在配置文件旁的 `.data/larkpush.sqlite` 中。设置 `[server].database_path` 可使用其他位置。

待处理登录和授权码等状态使用具有短有效期的*内存键值缓存*，由后台清理任务维护。这意味着服务器重启后，未完成的登录必须重新开始。此外，服务器不支持在负载均衡器或集群后运行多个实例。

<a name="token-lifetime-and-rotation"></a>
#### 令牌有效期与轮换

MCP 访问令牌有效期为一小时。`/token` 端点支持 `refresh_token` 授权方式，并可在最长 90 天未使用的期限内轮换刷新令牌，因此 OAuth 客户端无需再次通过飞书登录即可续期。此功能推出前建立的连接需要重新登录一次，以取得刷新令牌。

<a name="cloudflare-worker"></a>
### ⛅ Cloudflare Worker

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/Cnily03/larkpush-mcp)

本仓库还提供作为 Cloudflare Worker 运行的无服务器实现。

Worker 使用 [Cloudflare D1](https://developers.cloudflare.com/d1/) 和 [Upstash Redis](https://developers.cloudflare.com/workers/databases/third-party-integrations/upstash/)。部署 Worker 前需要先配置它们。

<a name="deploy-with-the-button"></a>
#### 使用按钮部署

请先[创建 Upstash 数据库](https://docs.upstash.com/redis#create-a-database)。

该按钮会导入仓库根目录及其中的共享 crates，并配置 D1 数据库。Worker 会在首次请求时初始化数据库结构。

- 创建或选择 D1 数据库。
- 根据不用的开发平台和服务区域，如果想切换 OpenAPI 端点，请手动设置或修改 OpenAPI 端点变量。
- 将 `PUBLIC_BASE_URL` 设置为 Worker 最终的 HTTPS 来源地址。
- 将 `LARK_APP_ID` 设置为飞书 **App ID**。
- 将 `LARK_APP_SECRET` 设置为飞书 **App Secret**，并作为密钥保存。
- 将 `REDIS_URL` 设置为 Upstash Redis URL，并作为密钥保存。其格式为 `rediss://default:password@host:6379`，可从 [Upstash 控制台](https://console.upstash.com)获取。

连接客户端前，别忘了在[飞书开放平台](https://open.feishu.cn)注册 `{PUBLIC_BASE_URL}/oauth/feishu/callback` 作为重定向 URL。如果最终 URL 是在部署后分配的，请更新 `PUBLIC_BASE_URL` 并重新部署。

<a name="manual-setup"></a>
#### 手动设置

安装 Wrangler、Rust WebAssembly 目标和 Rust 构建工具。

```bash
npm install -g wrangler
rustup target add wasm32-unknown-unknown
cargo install worker-build@^0.8
```

从仓库根目录进行身份验证并创建 D1 数据库：

```bash
wrangler login
wrangler d1 create larkpush
```

建议创建本地 Wrangler 配置和 `.env`：

```bash
cp wrangler.toml wrangler.local.toml
cp .env.example .env
```

在 `wrangler.local.toml` 中，将 `database_id` 设置为返回的数据库 UUID。

在 `[vars]` 中：

- 通过注释或取消注释相应条目，根据需要切换 OpenAPI 端点。
- 将 `PUBLIC_BASE_URL` 设置为 Worker 最终的 HTTPS 来源地址。
- 将 `LARK_APP_ID` 设置为飞书 **App ID**。

在 `.env` 中保存飞书 **App Secret** 和 Upstash Redis URL，将其上传到 Cloudflare 后部署：

```bash
wrangler -c wrangler.local.toml secret put LARK_APP_SECRET
wrangler -c wrangler.local.toml secret put REDIS_URL
wrangler -c wrangler.local.toml deploy
```

在[飞书开放平台](https://open.feishu.cn)注册 `{PUBLIC_BASE_URL}/oauth/feishu/callback` 作为重定向 URL。

<a name="local-development"></a>
#### 本地开发

将 `PUBLIC_BASE_URL` 设置为本地来源地址，例如 `http://localhost:8787`；然后在[飞书开放平台](https://open.feishu.cn)注册对应的重定向 URL（例如 `http://localhost:8787/oauth/feishu/callback`），再运行：

```bash
wrangler -c wrangler.local.toml dev --port 8787 # --var PUBLIC_BASE_URL:http://localhost:8787
```

> [!Note]
> 根目录的 `.env`、`wrangler.local.toml` 和构建输出均被 Git 忽略。`.env` 仅用于提供本地密钥。
>
> 已部署的 Worker 请使用 `wrangler secret put` 设置的密钥。
>
> 本地开发使用配置的 Upstash Redis 数据库，因此请使用与生产环境**分开**的数据库。
>

Worker 日志会在 `wrangler dev` 终端显示 UTC 时间、级别、范围和请求详情。查看已部署 Worker 的日志：

```bash
wrangler -c wrangler.local.toml tail --format pretty
```

<a name="migrate-to-cloudflare-worker"></a>
#### 迁移到 Cloudflare Worker

仓库提供迁移工具，可将原生服务器（CLI）的 SQLite 数据库复制到 Cloudflare D1 或本地 Worker 开发环境。

复制 SQLite 数据库文件前，请先停止原生服务器。

将 SQLite 数据库文件直接传给 `migrate-db`，它会执行 D1 初始化和迁移。

```bash
# 迁移到用于开发的本地 D1
cargo run --bin migrate-db -- -c wrangler.local.toml .data/larkpush.sqlite --local
# 迁移到用于生产的远程 D1
cargo run --bin migrate-db -- -c wrangler.local.toml .data/larkpush.sqlite --remote
```

两个目标默认都使用 `wrangler.toml`；使用 `-c` 可指定其他 Wrangler 文件。

请注意，迁移工具不会因配置差异而替换任何数据值。它只复制数据。

<a name="database-storage-1"></a>
#### 数据库存储

D1 数据库使用与原生服务器 SQLite 相同的表结构。

Worker 会在首次使用时自动初始化 D1 表结构。[迁移](#migrate-to-cloudflare-worker)也会在导入数据前执行初始化。

状态存储使用具有短有效期的外部 Redis 数据库，其中的数据会被自动消费或清理。

本地 D1 数据默认存储在 `.wrangler/state` 下，与远程数据库分开，而 Redis 并非如此。本地开发应使用单独的 Redis 数据库，以免与生产数据冲突。

作为无服务器应用，Worker 支持多个实例或集群部署。

<a name="send-a-message"></a>
## 💬 发送消息

`send_as_app` 接收 `receive_id`、`receive_id_type`、`msg_type` 和 `content` 中的 JSON 对象。例如：

```json
{"receive_id":"ou_...","receive_id_type":"open_id","msg_type":"text","content":{"text":"Hello"}}
```

发送到群聊时，使用 `receive_id_type: "chat_id"` 和该群的 `chat_id`。发送图片或文件前，先调用 `upload_image` 或 `upload_file`，再使用返回的资源 Key。有关富文本和其他消息类型，参见[飞书消息内容格式](https://open.feishu.cn/document/server-docs/im-v1/message-content-description/create_json)。

`send_multi_users` 会向多个用户排队发送相同的消息。它接收来自 `list_users` 或 `search_users` 的 Open ID，并返回批量消息 ID 和任何无效的 Open ID：

```json
{"open_ids":["ou_...","ou_..."],"msg_type":"text","content":{"text":"Hello"}}
```

当 `msg_type: "interactive"` 时，将卡片放在 `content.card` 中。批量消息为异步发送，不能发送到群聊，也不能绕过机器人的可用范围。

<a name="use-in-chatgpt"></a>
## 🤖 在 ChatGPT 中使用

在 [ChatGPT Plugins](https://chatgpt.com/plugins) 中添加带 OAuth 的 MCP URL。按照上面的示例，MCP URL 类似 `https://your-domain/mcp`。

输入 MCP URL 后，ChatGPT 会自动发现 OAuth 配置。如果未能发现，请检查反向代理或防火墙是否拦截了 `/.well-known/oauth*` 端点。

创建并安装插件，尝试使用飞书账号登录插件。然后开始新对话，让 ChatGPT 向飞书用户或群聊发送消息。

有关更多详情，请参阅官方文档：[Connect and test your plugin](https://developers.openai.com/plugins/deploy/connect-chatgpt)。

<a name="license"></a>
## 🎫 License

Copyright (c) Cnily03. All rights reserved.

Licensed under the [MIT License](LICENSE).
