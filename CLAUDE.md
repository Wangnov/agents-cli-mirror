# Agents CLI Mirror

## 项目概述

Rust 实现的 CLI 工具镜像服务，为 [Agents APP](https://github.com/example/agents-app) (Tauri 客户端) 提供后端支持，同时提供御三家（Claude Code、Codex、Gemini CLI）的安装脚本供用户手动安装。

## 技术栈

- **语言**: Rust (Edition 2024, MSRV 1.85)
- **HTTP 框架**: axum 0.8
- **异步运行时**: tokio
- **HTTP 客户端**: reqwest
- **序列化**: serde + serde_json + toml
- **SHA256 校验**: sha2 + hex
- **错误处理**: thiserror + anyhow

## 项目结构

```
agents-cli-mirror/
├── src/
│   ├── main.rs              # 入口点
│   ├── lib.rs               # 库导出
│   ├── config.rs            # 配置管理 (TOML + 环境变量)
│   ├── server.rs            # HTTP 服务器 (axum 路由)
│   ├── cache.rs             # 缓存管理 (版本清理、元数据)
│   ├── error.rs             # 错误类型定义
│   ├── retry.rs             # 请求重试机制
│   └── providers/
│       ├── mod.rs
│       ├── claude_code.rs   # Claude Code (GCS 同步)
│       ├── codex.rs         # Codex (GitHub Release)
│       ├── gemini.rs        # Gemini CLI (GitHub Release)
│       ├── node.rs          # Node.js (本地缓存验证)
│       └── node_pty.rs      # node-pty (本地缓存验证)
├── scripts/                 # 安装脚本 (include_str! 嵌入)
│   ├── claude-code-install.sh/ps1
│   ├── codex-install.sh/ps1
│   ├── gemini-install.sh/ps1
│   └── *-uninstall.sh/ps1
├── tests/
│   └── api_tests.rs         # 集成测试
├── .github/
│   ├── workflows/           # CI 工作流
│   └── scripts/             # E2E 测试脚本
├── docs/
│   └── plan.md              # 实现计划
├── config.toml              # 配置文件
├── config.toml.example      # 配置示例
└── cache/                   # 缓存目录 (gitignore)
```

## 常用命令

```bash
# 构建 & 运行构建产物
cargo run --release

# 显式指定配置文件运行
cargo run --config config.toml

# 测试
cargo test

# 代码质量检查 (必须通过)
cargo fmt --check
cargo clippy -- -D warnings

# 格式化
cargo fmt
```

## 代码规范

- **必须通过**: `cargo fmt` 和 `cargo clippy -- -D warnings`
- 提交前务必运行 `cargo fmt && cargo clippy -- -D warnings`
- 错误处理使用 `anyhow::Result` 和 `thiserror` 自定义错误

## Provider 设计模式

### 主动同步 Provider (Claude Code / Codex / Gemini)

从上游拉取并缓存：
1. `fetch_upstream_tag()` - 获取上游版本
2. `sync_tag()` - 同步指定标签
3. `sync_version()` - 下载并校验所有平台文件
4. `get_tag_version()` - 读取本地缓存的版本

### 被动验证 Provider (Node.js / node-pty)

文件通过 CI 构建后上传，Provider 仅验证：
1. 读取本地 `checksums.json`
2. 验证文件存在且大小匹配
3. 更新元数据

## 安装脚本规范

所有安装脚本位于 `scripts/` 目录，通过 `include_str!` 宏嵌入二进制。

**必须实现的特性**：
- 中英文国际化（检测 `$LANG` / `Get-UICulture`）
- `--tag`, `--version`, `--upgrade`, `--check`, `--no-modify-path` 参数
- SHA256 校验（从 manifest 或 `/api/*/checksums` 获取）
- 下载进度显示
- PATH 配置（符号链接优先，回退修改 shell 配置）

**占位符替换**：
- `__MIRROR_URL__` - 在 `server.rs` 的 `generate_*` 函数中替换为 `public_url`

## API 路由

| 路径模式 | 说明 |
|---------|------|
| `GET /{provider}/{tag}` | 获取版本号 |
| `GET /{provider}/{version}/...` | 下载文件 |
| `GET /{provider}/install.sh` | 安装脚本 |
| `GET /api/{provider}/info` | JSON 版本信息 |
| `GET /api/{provider}/checksums` | JSON 校验值 |
| `POST /api/{provider}/refresh` | 手动触发同步 |

## 配置

关键配置项 (`config.toml`):

```toml
[server]
public_url = "https://..."  # 必须配置，否则脚本接口返回 503

[cache]
max_versions = 10           # 保留的历史版本数

[update]
interval_minutes = 10       # 自动同步间隔
```

环境变量覆盖：`MIRROR_PORT`, `MIRROR_PUBLIC_URL`, `GITHUB_TOKEN` 等

## 测试

- **单元测试**: `src/` 中的 `#[cfg(test)]` 模块
- **集成测试**: `tests/api_tests.rs`
- **E2E 测试**: `.github/workflows/install-tests.yml` (7 平台矩阵)

## 注意事项

1. **同步互斥**: 所有同步操作通过 `sync_lock` 互斥，避免并发写入
2. **版本清理**: 保留被 tag 引用的版本，按时间清理旧版本
3. **校验失败处理**: 任一平台校验失败则不更新 tag，保留旧版本
