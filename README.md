# Agents CLI Mirror

`Agents CLI Mirror` 是一个以配置驱动为核心的制品镜像与分发系统。

它包含三个可发布二进制（两类产品角色）：

- `acm-server`：镜像服务端（拉取上游、缓存、分发、脚本入口）
- `acm-client`：本地客户端（命令 `acm`，建议通过包管理器安装）
- `acm-installer`：引导安装器（由脚本入口临时下载并执行，不作为日常主命令）

支持的存储后端只有两种：

- `local`（本地文件缓存）
- `s3`（任意 S3 兼容对象存储）

## 核心能力

- 动态 Provider：通过 `[[providers]]` 注册，不写死工具名
- 上游类型：`github_release` / `gcs_release` / `static`
- 统一下载路径：`/{provider}/{version}/files/{*filepath}`
- 统一脚本入口：
  - `/install/{provider}`
  - `/{provider}/install.sh`
  - `/update/{provider}`
  - `/uninstall/{provider}`
  - `/status`
  - `/doctor`
- 脚本协商策略：`Accept` 优先，`User-Agent` 兜底（自动返回 `sh` 或 `ps1`）

## 角色化使用指南

### 1) 作为镜像分发服务运营者

#### 第一步：准备配置

```toml
[server]
host = "0.0.0.0"
port = 1357
public_url = "https://mirror.example.com"
installer_provider = "installer"

[cache]
dir = "./cache"
max_versions = 10

[storage]
mode = "local"

[update]
enabled = true
interval_minutes = 10

[[providers]]
name = "codex"
source = "github_release"
repo = "openai/codex"
tags = ["latest"]
update_policy = "tracking"

[providers.ui]
preset = "codex"

[[providers]]
name = "installer"
source = "github_release"
repo = "wangnov/agents-cli-mirror"
tags = ["latest"]
update_policy = "tracking"

[providers.ui]
preset = "acm"
```

#### 第二步：启动服务

```bash
cargo run --release --bin acm-server -- serve --config config.toml
```

#### 第三步：验证接口

```bash
curl -fsSL http://127.0.0.1:1357/health
curl -fsSL http://127.0.0.1:1357/codex/latest
curl -fsSL http://127.0.0.1:1357/api/codex/info
curl -fsSL http://127.0.0.1:1357/api/codex/checksums
```

#### 第四步：对外分发脚本

```bash
# Unix shell
curl -fsSL https://mirror.example.com/install/codex | bash
curl -fsSL https://mirror.example.com/codex/install.sh | bash
curl -fsSL https://mirror.example.com/claude-code/install.sh | bash
curl -fsSL https://mirror.example.com/claude/install.sh | bash
curl -fsSL https://mirror.example.com/gemini/install.sh | bash

# PowerShell
irm https://mirror.example.com/install/codex | iex
irm https://mirror.example.com/install/claude-code | iex
irm https://mirror.example.com/install/gemini | iex
```

说明：上述脚本入口会先下载 `acm-installer`，再由它执行 `install/update/uninstall/status/doctor` 命令。

### 1.1) 纯云模式（GitHub Actions + R2 自定义域名，无 VPS）

目标：对外提供同一域名下的静态接口：

- `/{provider}/{tag}`（tag -> version）
- `/api/{provider}/info`、`/api/{provider}/versions`、`/api/{provider}/checksums`
- `/{provider}/install.sh`（脚本安装入口）
- `/{provider}/{version}/files/{*filepath}`（大文件必须 R2 直出，不经 Worker/VPS 代理）

核心点：制品对象在存储里的 key 形如 `/{provider}/versions/{version}/files/...`，但客户端对外下载路径是 `/{provider}/{version}/files/...`，因此需要在 Cloudflare 边缘做一次 URL Rewrite。

当前内置的公开 provider 名称是：

- `claude-code`
- `codex`
- `gemini`
- `installer`

注意：Claude 的规范 provider 名仍然是 `claude-code`；同时为了兼容用户习惯，公开安装入口也会额外发布 `/claude/*` 别名。

#### A) Cloudflare / R2 侧配置

1) 绑定 R2 Bucket 的自定义域名（例如 `https://mirror.example.com`），确保该域名直接指向 R2（不走任何 Worker 代理）。

2) 配置 Transform Rules / URL Rewrite（示例规则）：

- 匹配：`^/([^/]+)/([^/]+)/files/(.*)$`
- 重写为：`/$1/versions/$2/files/$3`

#### B) GitHub Actions 自动同步与发布

仓库已内置工作流：`.github/workflows/mirror-cloud-sync.yml`（定时 + 手动触发）。

你需要在仓库 Secrets 里配置（名称与代码一致）：

- `MIRROR_PUBLIC_URL`：对外域名（例如 `https://mirror.example.com`）
- `MIRROR_S3_ENDPOINT` / `MIRROR_S3_BUCKET`
- `MIRROR_S3_ACCESS_KEY_ID` / `MIRROR_S3_SECRET_ACCESS_KEY`
- 可选：`MIRROR_S3_REGION`（R2 通常用 `auto`）、`MIRROR_S3_PREFIX`

该工作流会执行两步：

- `acm-server sync`：拉取上游并上传制品到 R2
- `acm-server publish`：把 `/{provider}/{tag}`、`/api/{provider}/*`、`/{provider}/install.sh` 等“动态接口”发布成 R2 静态对象

本地手动跑（示例）：

```bash
cargo run --release --bin acm-server -- sync --config config.cloud.toml --provider all
cargo run --release --bin acm-server -- publish --config config.cloud.toml --provider all
```

#### C) refresh（可选：用 Worker 触发 workflow_dispatch）

如果你希望保留 `POST /api/{provider}/refresh` 这个入口（不自建 VPS），可以部署 `workers/refresh-dispatch`，并且只把路由绑定到 `/api/*/refresh`。

### 2) 作为个人用户（两种方式）

#### 方式 A：不自建镜像，只使用 `acm-client`

`acm-client`（二进制 `acm`）负责本机安装/更新/卸载。该模式通常通过包管理器安装 `acm-client`（如 crates.io/Homebrew/npm 包装）。

构建形态：

- `acm`（lite，默认）：不包含 `s3` 支持
- `acm-full`（二进制名 `acm-full`，开启 `s3` feature）：用于需要 `storage.mode = "s3"` 的场景

本地构建示例：

```bash
# lite
cargo build -p acm --release

# full (s3, outputs `target/release/acm-full`)
cargo build -p acm-full --release
```

可用三种来源策略（按优先级）：

- 显式镜像：`--mirror-url <url>`
- 环境变量镜像：`MIRROR_URL=<url>`
- 配置默认镜像：`[client].default_mirror_url = "https://..."`
- 配置驱动直连上游：当以上镜像都未配置时，按 `config.toml` 的 `providers` 配置直接同步上游并安装（需要本地可访问上游）

如果你在 lite 构建下使用了 `storage.mode = "s3"`，命令会提示“当前构建未启用 s3 feature”。

发布资产命名：

- `acm-<target>.*`：lite
- `acm-full-<target>.*`：full（含 s3）
- `acm-server-<target>.*`
- `acm-installer-<target>.*`

```bash
# 直接指定镜像地址
acm --mirror-url https://mirror.example.com install codex
acm --mirror-url https://mirror.example.com update codex
acm --mirror-url https://mirror.example.com uninstall codex

# 不指定 mirror，直接按 config.toml 从上游同步后安装
acm --config config.toml install codex
```

#### 方式 B：本机自部署（`acm-server` + `acm-client`）

`acm-server` 负责拉取上游并提供镜像，`acm-client` 负责在本机安装和管理工具。

```bash
# 1) 先启动本机镜像服务
cargo run --release --bin acm-server -- serve --config config.toml

# 2) 再用本机客户端操作（指向本机 mirror）
acm --mirror-url http://127.0.0.1:1357 install codex
acm --mirror-url http://127.0.0.1:1357 update codex
acm --mirror-url http://127.0.0.1:1357 uninstall codex
```

状态与诊断：

```bash
acm status
acm status --provider codex
acm doctor
```

自动更新控制：

```bash
acm auto-update enable codex
acm auto-update disable codex
acm auto-update status
acm auto-update run
```

## 配置说明

### `[server]`

- `host` / `port`：监听地址
- `public_url`：脚本生成使用的外部地址；未配置时脚本接口返回 `503`
- `installer_provider`：脚本入口下载 `acm-installer` 时使用的 provider 名称（默认 `installer`）
- `refresh_token`：`POST /api/{provider}/refresh` 的 Bearer Token
- `refresh_min_interval_seconds`：refresh 节流窗口

### `[client]`

- `default_mirror_url`：`acm` / `acm-installer` 的默认镜像地址（仅当未传 `--mirror-url` 且未设置 `MIRROR_URL` 时生效）

### `[storage]`

- `mode = "local"`：文件落本地缓存目录
- `mode = "s3"`：下载后上传到 S3 兼容存储并通过预签名 URL 分发

### `[[providers]]`

- `name`：provider 唯一名（小写字母/数字/`-`/`_`/`.`）
- `source`：`github_release` / `gcs_release` / `static`
- `tags`：标签列表（如 `latest` / `stable` / `v1.2.3`）
- `update_policy`：`tracking` / `pinned` / `manual`
- `repo`：`github_release` 必填
- `upstream_url`：`gcs_release` 必填
- `static_version`：`static` 必填
- `files`：
  - `github_release` 可留空（镜像全部 release assets）
  - `gcs_release` / `static` 必填

### `[providers.ui]`

- `preset`：installer 终端 UI 主题，支持 `acm` / `codex` / `claude` / `gemini`
- 默认值：`acm`
- 生效范围：`acm install`、`acm update`、`acm uninstall`
- 完成态规则：安装完成/更新完成/卸载完成不会强制切成绿色，会保持该 provider 的主题色

示例：

```toml
[[providers]]
name = "claude-code"
source = "gcs_release"
upstream_url = "https://storage.googleapis.com/your-bucket/releases"
tags = ["latest"]
update_policy = "tracking"
files = ["linux-x64/claude", "darwin-arm64/claude"]

[providers.ui]
preset = "claude"
```

## HTTP API

| Method | Path | 说明 |
|---|---|---|
| GET | `/health` | 健康检查 |
| GET | `/{provider}/{tag}` | 查询 tag -> version |
| GET | `/{provider}/{version}/files/{*filepath}` | 下载制品 |
| GET | `/api/{provider}/info` | Provider 信息/同步状态 |
| GET | `/api/{provider}/versions` | 已缓存版本 |
| GET | `/api/{provider}/checksums` | 校验信息 |
| POST | `/api/{provider}/refresh` | 手动同步 |
| GET | `/install/{provider}` | 安装脚本入口（自动协商 sh/ps1） |
| GET | `/update/{provider}` | 更新脚本入口 |
| GET | `/uninstall/{provider}` | 卸载脚本入口 |
| GET | `/status` | 状态脚本入口 |
| GET | `/doctor` | 诊断脚本入口 |

## 开发命令

```bash
cargo fmt
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

## 当前约束

- `server.public_url` 是脚本分发的前置条件
- PowerShell 路径下，installer 目前要求 `.zip` 或 `.exe` 资产格式

## License

AGPL-3.0
