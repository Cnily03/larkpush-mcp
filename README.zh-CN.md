# Larkpush

[English](README.md) | **简体中文**

`larkpush` 是一个受 OAuth 保护的 MCP 服务器，用于查找 Feishu（飞书）用户和群聊，并以飞书应用机器人的身份发送消息。

## 📄 目录

- [快速开始](#get-started)
  - [创建飞书应用](#创建飞书应用)
  - [工具与权限](#工具与权限)
- [部署](#deployment)
  - [CLI](#cli)
  - [Cloudflare Worker](#cloudflare-worker)
- [发送消息](#send-a-message)
- [在 ChatGPT 中使用](#use-in-chatgpt)
- [许可证](#license)

<a id="get-started"></a>

## 💡 快速开始

### 创建飞书应用

1. 打开[飞书开放平台开发者后台](https://open.feishu.cn/app)。
2. 创建企业自建应用，并启用机器人能力。
3. 为应用开通 [MCP 工具所需的应用身份权限](#工具与权限)。
4. 发布应用版本，并保存 App ID 和 App Secret，以便部署时使用。

### 工具与权限

在飞书开发者后台开通以下**应用身份**权限。组织架构工具仅返回 Open ID，不请求部门或个人资料字段。

| MCP 工具 | 用途 | 飞书权限 | 飞书 API |
| --- | --- | --- | --- |
| `list_users` | 列出应用可见范围内的员工及其 Open ID。 | `directory:employee:list` | [批量获取员工列表](https://open.feishu.cn/document/directory-v1/employee/filter) |
| `search_users` | 搜索应用可见范围内的员工并返回 Open ID。 | `directory:employee:search` | [搜索员工](https://open.feishu.cn/document/directory-v1/employee/search) |
| `list_chats` | 列出机器人所在的群聊。 | `im:chat:read` 或 `im:chat:readonly` | [获取用户或机器人所在的群列表](https://open.feishu.cn/document/server-docs/group/chat/list) |
| `list_chat_members` | 列出机器人所在群聊的成员。 | `im:chat.members:read` 或 `im:chat:readonly` | [获取群成员列表](https://open.feishu.cn/document/server-docs/group/chat-member/get) |
| `send_as_app` | 以机器人身份发送文本、富文本、图片、卡片或文件。 | `im:message:send_as_bot` 或 `im:message` | [发送消息](https://open.feishu.cn/document/server-docs/im-v1/message/create) |
| `send_multi_users` | 将同一条消息加入发送给多个用户的队列。 | `im:message:send_multi_users` 和 `im:message:send_as_bot`（或 `im:message`） | [批量发送消息](https://open.feishu.cn/document/server-docs/im-v1/batch_message/send-messages-in-batches) |
| `upload_image` | 上传图片并返回 `image_key`。 | `im:resource` | [上传图片](https://open.feishu.cn/document/server-docs/im-v1/image/create) |
| `upload_file` | 上传文件并返回 `file_key`。 | `im:resource` | [上传文件](https://open.feishu.cn/document/server-docs/im-v1/file/create) |

`list_users` 和 `search_users` 使用飞书组织架构（Directory）API，返回包含 `open_id` 的 `items`，以及用于分页的 `has_more` 和 `next_page_token`。获取姓名需要额外的字段权限，例如 `directory:employee.base.name.name:read`，因此当前权限配置下不会请求姓名。机器人必须已加入目标群聊，单聊消息的接收人则必须在应用的可用范围内。可选配置 `[lark].lark_scope` 用于用户的 OAuth 登录，与上述应用身份权限相互独立。

<a id="deployment"></a>

## 🚀 部署

可以选择原生 CLI 服务器或 Cloudflare Worker。

<a id="cli"></a>

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

构建或安装 CLI。这会生成 CLI 程序 `larkpush`，以及用于[迁移至 Cloudflare Worker](#迁移至-cloudflare-worker) 的 `migrate-db` 程序：

```bash
# 构建 CLI
cargo build --release
# 或安装到 $HOME/.cargo/bin
cargo install --path .
```

配置步骤：

配置字段、环境变量和回调路径沿用程序中的原始名称，例如 `[lark]`、`lark_authorize_url`、`LARK_APP_ID` 和 `/oauth/feishu/callback`，无需因文档语言而重命名。

1. 将 `config.example.toml` 复制为 `config.toml`。
2. 在 `[lark]` 中设置 **App ID** 和 **App Secret**。
   在[飞书开发者后台](https://open.feishu.cn/app)中打开应用的“凭证与基础信息”页面获取。
3. 将 `[server].public_base_url` 设置为服务对外提供访问的 HTTPS 源地址，例如 `https://your-domain.com`。
4. 在[飞书开发者后台](https://open.feishu.cn/app)的“安全设置 → 重定向 URL”中添加 `https://your-domain/oauth/feishu/callback`，其源地址须与 `public_base_url` 一致。

按照上述示例，MCP URL 为 `https://your-domain/mcp`。

> [!Caution]
> 请妥善保管 `config.toml`，不要将其提交到公开仓库。

运行原生 Rust 服务器：

```bash
target/release/larkpush mcp -L -c config.toml # 通过构建生成
larkpush mcp -L -c config.toml                # 通过安装生成
```

`-L` 默认使用 `[server].listen`；可通过 `-L=0.0.0.0:3000` 覆盖监听地址。建议通过 HTTPS 对外提供服务，并使用反向代理转发服务器的所有路由。

#### 数据库存储

原生服务器使用 SQLite 保存 OAuth 客户端和令牌授权等持久化数据，默认路径为配置文件所在目录下的 `.data/larkpush.sqlite`。可通过 `[server].database_path` 指定其他位置。

待完成的登录和授权码等临时状态保存在具有较短过期时间的*内存键值缓存*中，由后台清理任务处理过期数据。因此，服务器重启后，尚未完成的登录需要重新开始。原生服务器不支持负载均衡后的多实例部署或集群部署。

#### 令牌有效期与轮换

MCP 访问令牌的有效期为一小时。`/token` 端点支持 `refresh_token` 授权类型，通过刷新令牌轮换，允许最长 90 天不活跃期，使 OAuth 客户端无需再次登录飞书即可续期访问。此功能推出前建立的连接需要重新登录一次，以获取刷新令牌。

<a id="cloudflare-worker"></a>

### ⛅ Cloudflare Worker

[![部署到 Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/Cnily03/larkpush-mcp)

本仓库同时提供基于 Cloudflare Worker 的无服务器实现。

Worker 使用 [Cloudflare D1](https://developers.cloudflare.com/d1/) 和 [Upstash Redis](https://developers.cloudflare.com/workers/databases/third-party-integrations/upstash/)，部署前需要完成相应配置。

#### 使用按钮部署

请先[创建 Upstash 数据库](https://docs.upstash.com/redis#create-a-database)。

部署按钮会导入仓库根目录（包含共享 crate），并配置 D1 数据库。Worker 会在首次请求时初始化数据库结构。

- 创建或选择 D1 数据库。
- 将 `PUBLIC_BASE_URL` 设置为 Worker 最终使用的 HTTPS 源地址。
- 将 `LARK_APP_ID` 设置为飞书应用的 **App ID**。
- 将飞书应用的 **App Secret** 作为密钥保存到 `LARK_APP_SECRET`。
- 将 Upstash Redis URL 作为密钥保存到 `REDIS_URL`。从 [Upstash 控制台](https://console.upstash.com)获取，格式为 `rediss://default:password@host:6379`。

连接客户端前，请在[飞书开发者后台](https://open.feishu.cn/app)将 `{PUBLIC_BASE_URL}/oauth/feishu/callback` 注册为重定向 URL。如果最终 URL 在部署后才确定，请更新 `PUBLIC_BASE_URL` 并重新部署。

#### 手动配置

安装 Wrangler、Rust WebAssembly 编译目标及 Rust 构建工具：

```bash
npm install -g wrangler
rustup target add wasm32-unknown-unknown
cargo install worker-build@^0.8
```

在仓库根目录下登录并创建 D1 数据库：

```bash
wrangler login
wrangler d1 create larkpush
```

建议创建本地 Wrangler 配置文件和 `.env`：

```bash
cp wrangler.toml wrangler.local.toml
cp .env.example .env
```

在 `wrangler.local.toml` 中，将 `database_id` 设置为上一步返回的数据库 UUID。

在 `[vars]` 中：

- 将 `PUBLIC_BASE_URL` 设置为 Worker 最终使用的 HTTPS 源地址。
- 将 `LARK_APP_ID` 设置为飞书应用的 **App ID**。

将飞书应用的 **App Secret** 和 Upstash Redis URL 保存到 `.env`，再上传密钥至 Cloudflare 并部署：

```bash
wrangler -c wrangler.local.toml secret put LARK_APP_SECRET
wrangler -c wrangler.local.toml secret put REDIS_URL
wrangler -c wrangler.local.toml deploy
```

在[飞书开发者后台](https://open.feishu.cn/app)将 `{PUBLIC_BASE_URL}/oauth/feishu/callback` 注册为重定向 URL。

#### 本地开发

将 `PUBLIC_BASE_URL` 设置为本地源地址，例如 `http://localhost:8787`，并在[飞书开发者后台](https://open.feishu.cn/app)注册对应的重定向 URL（例如 `http://localhost:8787/oauth/feishu/callback`），然后运行：

```bash
wrangler -c wrangler.local.toml dev --port 8787 # --var PUBLIC_BASE_URL:http://localhost:8787
```

> [!Note]
> 根目录下的 `.env`、`wrangler.local.toml` 和构建产物均已被 Git 忽略。`.env` 仅提供本地密钥。
>
> 已部署的 Worker 使用通过 `wrangler secret put` 设置的密钥。
>
> 本地开发会使用配置中的 Upstash Redis 数据库，请使用与生产环境**独立**的数据库。

在 `wrangler dev` 终端中，Worker 日志会显示 UTC 时间、日志级别、作用域及请求详情。查看已部署 Worker 的日志：

```bash
wrangler -c wrangler.local.toml tail --format pretty
```

#### 迁移至 Cloudflare Worker

仓库提供迁移工具，可将原生 CLI 服务器的 SQLite 数据库复制到 Cloudflare D1 或本地 Worker 开发环境。

复制 SQLite 数据库文件前，请先停止原生服务器。

将 SQLite 数据库文件直接传给 `migrate-db`，工具会执行 D1 初始化和数据迁移：

```bash
# 迁移到本地 D1，用于开发
cargo run --bin migrate-db -- -c wrangler.local.toml .data/larkpush.sqlite --local
# 迁移到远程 D1，用于生产
cargo run --bin migrate-db -- -c wrangler.local.toml .data/larkpush.sqlite --remote
```

无论迁移到本地还是远程，默认配置文件均为 `wrangler.toml`；可使用 `-c` 指定其他 Wrangler 配置文件。

迁移工具仅复制数据，不会根据配置差异改写数据中的值。

#### 数据库存储

D1 数据库与原生服务器中的 SQLite 使用相同的数据库结构。

Worker 会在首次使用时自动初始化 D1 数据库结构。[迁移工具](#迁移至-cloudflare-worker)也会在导入数据前完成初始化。

临时状态保存在外部 Redis 数据库中，具有较短的过期时间，会在使用后删除或自动清理。

本地 D1 数据默认保存在 `.wrangler/state` 下，与远程数据库相互独立；Redis 则不会自动隔离。请为本地开发使用独立的 Redis 数据库，避免与生产环境冲突。

作为无服务器应用，Worker 支持多实例或集群部署。

<a id="send-a-message"></a>

## 💬 发送消息

`send_as_app` 接收 `receive_id`、`receive_id_type`、`msg_type`，以及以 JSON 对象形式传入的 `content`。例如：

```json
{"receive_id":"ou_...","receive_id_type":"open_id","msg_type":"text","content":{"text":"Hello"}}
```

向群聊发送消息时，使用 `receive_id_type: "chat_id"` 和目标群的 `chat_id`。发送图片或文件前，先调用 `upload_image` 或 `upload_file`，再使用返回的 key 发送消息。富文本及其他消息类型请参阅[飞书消息内容格式](https://open.feishu.cn/document/server-docs/im-v1/message-content-description/create_json)。

`send_multi_users` 将同一条消息加入发送给多个用户的队列，接收 `list_users` 或 `search_users` 返回的 Open ID，并返回批量消息 ID 和无效的 Open ID：

```json
{"open_ids":["ou_...","ou_..."],"msg_type":"text","content":{"text":"Hello"}}
```

当 `msg_type: "interactive"` 时，将卡片对象放在 `content.card` 中。批量消息异步发送，不支持发送到群聊，也无法绕过机器人的可用范围限制。

<a id="use-in-chatgpt"></a>

## 🤖 在 ChatGPT 中使用

在 [ChatGPT Plugins](https://chatgpt.com/plugins) 中添加 MCP URL，并使用 OAuth 认证。按照前面的示例，MCP URL 为 `https://your-domain/mcp`。

输入 MCP URL 后，ChatGPT 会自动发现 OAuth 配置。如果未能发现，请检查反向代理或防火墙是否阻止了 `/.well-known/oauth*` 端点。

创建并安装插件，然后使用飞书账号登录。接着新建会话，让 ChatGPT 向飞书用户或群聊发送消息。

更多信息请参阅官方文档：[连接并测试插件](https://developers.openai.com/plugins/deploy/connect-chatgpt)。

<a id="license"></a>

## 🎫 许可证

Copyright (c) Cnily03. All rights reserved.

本项目采用 [MIT 许可证](LICENSE)。
