# Agents CLI Mirror

Agents CLI Mirror 是 Claude Code 和 Codex CLI 的纯静态镜像。仓库里不再运行自建后端服务，镜像产物由 GitHub Actions 定时生成并上传到对象存储，用户安装时通过 Cloudflare Worker 路由到最快的下载源。

当前镜像的 provider 只有：

- `codex`
- `claude`

## 快速安装

Unix shell：

```bash
curl -fsSL https://install.agentsmirror.com/codex/install.sh | bash
curl -fsSL https://install.agentsmirror.com/claude/install.sh | bash
```

Windows PowerShell：

```powershell
irm https://install.agentsmirror.com/codex/install.ps1 | iex
irm https://install.agentsmirror.com/claude/install.ps1 | iex
```

安装脚本支持这些常用参数：

```bash
curl -fsSL https://install.agentsmirror.com/claude/install.sh | bash -s -- --install-dir ~/.local/bin
```

```powershell
irm https://install.agentsmirror.com/codex/install.ps1 | iex
# 或先下载脚本后传入：
# .\codex.ps1 --install-dir "$env:LOCALAPPDATA\Programs\codex"
```

## 架构

`.github/workflows/mirror.yml` 是镜像同步工作流，支持手动触发。Cloudflare Cron 每 15 分钟触发一次，GitHub 的 `11 */6 * * *` 定时任务作为后备。每次运行会按 provider 独立处理：

1. 生成 manifest：
   - `codex` 从 GitHub Releases `openai/codex` 读取最新 release，只镜像 6 个可安装 CLI 归档。
   - `claude` 从 Claude Code GCS release bucket 读取 `/latest` 文本指针，再读取 `<version>/manifest.json` 里的 checksum 和 size。
2. 清理旧版本，为新版本腾出空间；保留 R2 当前版本和二级 S3 自己的 `latest.json` 引用的版本。
3. 下载缺失的产物并校验 SHA256。
4. 上传到 Cloudflare R2 和 IHEP 二级 S3。默认要求二级 S3 同步成功，缺失配置或上传失败会使工作流失败。
5. 再次 prune；两侧同步完成后，只保留当前版本、安装脚本和 `latest.json`。
6. 从 `https://install.agentsmirror.com/<provider>/latest.json` 拉取并校验线上版本。

下载入口由 `cloudflare/download-router/` 里的 Worker 提供。它只服务 `/codex/` 和 `/claude/` 路径：

- 默认把请求 302 到 `GLOBAL_MIRROR_BASE_URL` 指向的 R2 公网域名。
- 当访问国家在 `SECONDARY_COUNTRY_CODES` 中且二级 S3 凭据完整时，Worker 先检查可用性，再生成 IHEP 预签名 URL 并 302 到该地址。安装包只检查 HEAD，文件正文直接由存储服务下载。
- 二级源缺文件、请求失败或 3 秒内无响应时，回退到 R2；`latest.json` 还会比较版本和平台校验信息，防止安装停更的旧版本。
- 默认中国大陆流量匹配 `CN`，预签名 URL 默认有效期是 3600 秒。

## 对象布局

所有对象都按同一套静态 key 发布：

```text
<provider>/latest.json
<provider>/<version>/<key>/<file>
<provider>/install.sh
<provider>/install.ps1
```

示例：

```text
codex/latest.json
codex/<version>/x86_64-apple-darwin/codex-x86_64-apple-darwin.tar.gz
claude/latest.json
claude/<version>/darwin-arm64/claude
claude/install.sh
```

manifest 结构为：

```json
{
  "provider": "claude",
  "version": "1.0.0",
  "published_at": "2026-06-09T00:00:00Z",
  "platforms": {
    "darwin-arm64": {
      "file": "claude",
      "sha256": "...",
      "size": 123,
      "bin": "claude"
    }
  }
}
```

## 关键脚本

- `scripts/build-codex-manifest.sh`：从 `openai/codex` release API 生成 `codex/latest.json`。
- `scripts/download-codex.sh`：下载 Codex release 资产并校验 manifest 里的 SHA256。
- `scripts/build-claude-manifest.sh`：从 Claude Code GCS `/latest` 和 upstream manifest 生成 `claude/latest.json`；8 个 Claude 平台内置在脚本中。
- `scripts/download-claude.sh`：从 Claude Code GCS 下载 manifest 中的二进制并校验 SHA256。
- `scripts/sync-r2.sh`：上传 artifact tree 和 `install/` 脚本到 R2。
- `scripts/sync-secondary-s3.sh`：上传同一份 artifact tree 和 `install/` 脚本到 IHEP 二级 S3。
- `scripts/prune.sh`：以 R2 的 `latest.json` 为准，清理 R2 和已配置的二级 S3 旧版本。

## GitHub Secrets

完整生产同步需要配置这些 repository secrets：

R2：

- `MIRROR_S3_ENDPOINT`
- `MIRROR_S3_ACCESS_KEY_ID`
- `MIRROR_S3_SECRET_ACCESS_KEY`
- `MIRROR_S3_BUCKET`，未配置时脚本默认使用 `agentclimirror`

IHEP 二级 S3：

- `SECONDARY_S3_ENDPOINT`
- `SECONDARY_S3_BUCKET`
- `SECONDARY_S3_ACCESS_KEY_ID`
- `SECONDARY_S3_SECRET_ACCESS_KEY`
- `SECONDARY_S3_REGION`，未配置时脚本默认使用 `us-east-1`

工作流默认要求二级 S3 成功。只使用 R2 的部署可以设置 repository variable `SECONDARY_S3_REQUIRED=false`，允许缺失二级源或上传失败；已配置的二级源仍会检查完整性并清理旧版本。

`scripts/prune.sh` 支持 `PRUNE_DRY_RUN=true`，列出清理候选而不删除。清理二级源时，同时保护 R2 当前版本和二级源仍在发布的版本；如果二级 manifest 无法读取或无效，会停止二级清理。

## Worker 配置

`cloudflare/download-router/wrangler.jsonc` 中的默认 vars：

- `GLOBAL_MIRROR_BASE_URL=https://r2.wangnov-ai.com`
- `SECONDARY_COUNTRY_CODES=CN`
- `SECONDARY_S3_SIGNED_URL_TTL_SECONDS=3600`

Worker secrets：

- `SECONDARY_S3_ENDPOINT`
- `SECONDARY_S3_BUCKET`
- `SECONDARY_S3_ACCESS_KEY_ID`
- `SECONDARY_S3_SECRET_ACCESS_KEY`
- `SECONDARY_S3_REGION`，可选，未设置时 Worker 默认使用 `us-east-1`

部署后，`install.agentsmirror.com/*` 路由到该 Worker。

## 本地验证

```bash
npm test
for script in scripts/*.sh install/*.sh; do bash -n "$script"; done
```

回归测试覆盖二级源失效/停更的 R2 回退、GET/HEAD 签名、旧版本清理和同步失败策略。需要 Node.js 24、Python 3 和 Bash，无需安装 npm 依赖。

```bash
rm -f /tmp/verify-claude.json
bash scripts/build-claude-manifest.sh /tmp/verify-claude.json
python3 -m json.tool /tmp/verify-claude.json >/dev/null
bash -n scripts/*.sh
```

同步相关脚本需要 AWS CLI 和对应 S3 凭据。普通 manifest 生成和下载脚本只需要 `bash`、`curl`、`python3`，以及 `sha256sum` 或 `shasum`。
