use axum::http::{HeaderMap, StatusCode, header};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptFlavor {
    Sh,
    Ps1,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ScriptCommand {
    Install,
    Update,
    Uninstall,
    Status,
    Doctor,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ScriptResolutionContext<'a> {
    pub accept: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub provider: Option<&'a str>,
}

impl ScriptFlavor {
    pub(crate) fn content_type(self) -> &'static str {
        match self {
            ScriptFlavor::Sh => "text/x-shellscript",
            ScriptFlavor::Ps1 => "text/plain",
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ScriptFlavor::Sh => "sh",
            ScriptFlavor::Ps1 => "ps1",
        }
    }
}

impl ScriptCommand {
    pub(super) fn as_cli(self) -> &'static str {
        match self {
            ScriptCommand::Install => "install",
            ScriptCommand::Update => "update",
            ScriptCommand::Uninstall => "uninstall",
            ScriptCommand::Status => "status",
            ScriptCommand::Doctor => "doctor",
        }
    }

    pub(super) fn needs_provider(self) -> bool {
        matches!(
            self,
            ScriptCommand::Install | ScriptCommand::Update | ScriptCommand::Uninstall
        )
    }
}

pub(super) fn negotiate_flavor(headers: &HeaderMap) -> ScriptFlavor {
    resolve_script_variant(ScriptResolutionContext {
        accept: header_text(headers, header::ACCEPT),
        user_agent: header_text(headers, header::USER_AGENT),
        provider: None,
    })
}

pub(super) fn resolve_script_variant(ctx: ScriptResolutionContext<'_>) -> ScriptFlavor {
    let _ = ctx.provider;
    if let Some(accept) = ctx.accept
        && let Some(flavor) = resolve_from_accept_header(accept)
    {
        return flavor;
    }

    if let Some(ua) = ctx.user_agent
        && contains_any(ua, &["powershell", "pwsh"])
    {
        return ScriptFlavor::Ps1;
    }

    ScriptFlavor::Sh
}

pub(crate) fn render_bootstrap_script(
    command: ScriptCommand,
    provider: Option<&str>,
    flavor: ScriptFlavor,
    mirror_url: Option<&str>,
    installer_provider: &str,
    installer_bin: &str,
) -> Result<String, StatusCode> {
    if command.needs_provider() && provider.is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let Some(mirror_url) = mirror_url else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };

    let provider = provider.unwrap_or_default();

    let script = match flavor {
        ScriptFlavor::Sh => render_sh(
            command,
            provider,
            mirror_url,
            installer_provider,
            installer_bin,
        ),
        ScriptFlavor::Ps1 => render_ps1(
            command,
            provider,
            mirror_url,
            installer_provider,
            installer_bin,
        ),
    };
    Ok(script)
}

fn header_text(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    needles.iter().any(|needle| lower.contains(needle))
}

fn resolve_from_accept_header(accept: &str) -> Option<ScriptFlavor> {
    let mut best: Option<(u16, ScriptFlavor)> = None;
    for raw_part in accept.split(',') {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }

        let (media_type_raw, params) = part.split_once(';').map_or((part, ""), |(m, p)| (m, p));
        let media_type = media_type_raw.trim().to_ascii_lowercase();
        let flavor = match media_type.as_str() {
            "application/x-powershell" | "text/x-powershell" | "application/powershell" => {
                Some(ScriptFlavor::Ps1)
            }
            "text/x-shellscript" | "application/x-shellscript" | "application/x-sh" => {
                Some(ScriptFlavor::Sh)
            }
            _ => None,
        };
        let Some(flavor) = flavor else {
            continue;
        };

        let quality = parse_accept_quality(params).unwrap_or(1000);
        if quality == 0 {
            continue;
        }

        match best {
            Some((current, _)) if current >= quality => {}
            _ => best = Some((quality, flavor)),
        }
    }
    best.map(|(_, flavor)| flavor)
}

fn parse_accept_quality(params: &str) -> Option<u16> {
    for raw in params.split(';') {
        let item = raw.trim();
        let Some(value) = item.strip_prefix("q=") else {
            continue;
        };
        let parsed = value.parse::<f32>().ok()?;
        if !(0.0..=1.0).contains(&parsed) {
            return None;
        }
        return Some((parsed * 1000.0).round() as u16);
    }
    None
}

fn render_sh(
    command: ScriptCommand,
    provider: &str,
    mirror_url: &str,
    installer_provider: &str,
    installer_bin: &str,
) -> String {
    let provider_line = if command.needs_provider() {
        format!("COMMAND_ARGS+=(\"{}\")", provider)
    } else {
        String::new()
    };

    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

MIRROR_URL="${{MIRROR_URL:-{mirror_url}}}"
INSTALLER_PROVIDER="${{INSTALLER_PROVIDER:-{installer_provider}}}"
INSTALLER_TAG="${{INSTALLER_TAG:-latest}}"
INSTALLER_VERSION="${{INSTALLER_VERSION:-}}"
BIN_NAME="{installer_bin}"
DEFAULT_CACHE_ROOT="${{XDG_CACHE_HOME:-$HOME/.cache}}/acm-installer"
CACHE_ROOT="${{ACM_INSTALLER_CACHE_DIR:-$DEFAULT_CACHE_ROOT}}"

FORWARD_ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --mirror-url)
            MIRROR_URL="$2"
            shift 2
            ;;
        --installer-tag)
            INSTALLER_TAG="$2"
            shift 2
            ;;
        --installer-version)
            INSTALLER_VERSION="$2"
            shift 2
            ;;
        *)
            FORWARD_ARGS+=("$1")
            shift
            ;;
    esac
done

if [[ -z "$MIRROR_URL" ]]; then
    echo "MIRROR_URL is required" >&2
    exit 1
fi

if command -v python3 >/dev/null 2>&1; then
    PYTHON="python3"
elif command -v python >/dev/null 2>&1; then
    PYTHON="python"
else
    echo "python3 (or python) is required" >&2
    exit 1
fi

detect_platform() {{
    local os arch libc=""
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"
    case "$os" in
        darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
            esac
            ;;
        linux)
            if ldd --version 2>&1 | grep -qi musl; then
                libc="-musl"
            else
                libc="-gnu"
            fi
            case "$arch" in
                x86_64) echo "x86_64-unknown-linux${{libc}}" ;;
                aarch64|arm64) echo "aarch64-unknown-linux${{libc}}" ;;
                *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
            esac
            ;;
        *)
            echo "Unsupported OS: $os" >&2
            exit 1
            ;;
    esac
}}

extract_installer() {{
    local archive="$1"
    local out_dir="$2"

    case "$archive" in
        *.zip)
            if command -v unzip >/dev/null 2>&1; then
                unzip -q "$archive" -d "$out_dir"
            else
                "$PYTHON" - "$archive" "$out_dir" <<'PY'
import sys, zipfile
archive, out_dir = sys.argv[1:3]
with zipfile.ZipFile(archive, 'r') as zf:
    zf.extractall(out_dir)
PY
            fi
            ;;
        *.tar.gz|*.tgz)
            tar -xzf "$archive" -C "$out_dir"
            ;;
        *.tar.xz)
            tar -xJf "$archive" -C "$out_dir"
            ;;
        *)
            ;;
    esac

    local found
    found="$(find "$out_dir" -type f \( -name "$BIN_NAME" -o -name "$BIN_NAME.exe" \) -print -quit)"
    if [[ -z "$found" && -f "$archive" ]]; then
        found="$archive"
    fi
    if [[ -z "$found" ]]; then
        echo "Installer binary not found" >&2
        exit 1
    fi
    echo "$found"
}}

url_encode_path() {{
    "$PYTHON" - "$1" <<'PY'
import sys
from urllib.parse import quote
value = sys.argv[1]
print('/'.join(quote(seg, safe='') for seg in value.split('/')))
PY
}}

calc_sha256() {{
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{{print $1}}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{{print $1}}'
    else
        echo "sha256sum or shasum is required" >&2
        exit 1
    fi
}}

PLATFORM="$(detect_platform)"

if [[ -z "$INSTALLER_VERSION" ]]; then
    INSTALLER_VERSION="$(curl -fsSL "$MIRROR_URL/$INSTALLER_PROVIDER/$INSTALLER_TAG" | tr -d '\r\n')"
fi

CHECKSUMS_JSON="$(curl -fsSL "$MIRROR_URL/api/$INSTALLER_PROVIDER/checksums")"
readarray -t INSTALLER_META < <("$PYTHON" - "$CHECKSUMS_JSON" "$INSTALLER_VERSION" "$PLATFORM" <<'PY'
import json, sys
checksums = json.loads(sys.argv[1])
version = sys.argv[2]
platform = sys.argv[3]
versions = checksums.get(version)
if not isinstance(versions, dict):
    raise SystemExit('installer version not found in checksums')
entry = versions.get(platform) or versions.get('universal')
if entry is None and versions:
    entry = next(iter(versions.values()))
if not isinstance(entry, dict):
    raise SystemExit('invalid checksum entry')
files = entry.get('files')
if isinstance(files, dict) and files:
    filename = sorted(files.keys())[0]
    sha256 = files.get(filename, {{}}).get('sha256') or entry.get('sha256')
else:
    filename = entry.get('filename')
    sha256 = entry.get('sha256')
if not filename or not sha256:
    raise SystemExit('installer filename or sha256 missing')
print(filename)
print(sha256)
PY
)

INSTALLER_FILE="${{INSTALLER_META[0]:-}}"
EXPECTED_SHA256="${{INSTALLER_META[1]:-}}"
if [[ -z "$INSTALLER_FILE" || -z "$EXPECTED_SHA256" ]]; then
    echo "failed to resolve installer metadata" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d)"
cleanup() {{ rm -rf "$TMP_DIR"; }}
trap cleanup EXIT

ENCODED_FILE="$(url_encode_path "$INSTALLER_FILE")"
ARCHIVE_BASENAME="$(basename "$INSTALLER_FILE")"
CACHE_ARCHIVE_DIR="$CACHE_ROOT/$INSTALLER_PROVIDER/$INSTALLER_VERSION/$PLATFORM"
mkdir -p "$CACHE_ARCHIVE_DIR"
CACHE_ARCHIVE="$CACHE_ARCHIVE_DIR/$ARCHIVE_BASENAME"

if [[ -f "$CACHE_ARCHIVE" ]]; then
    CACHED_SHA256="$(calc_sha256 "$CACHE_ARCHIVE")"
    if [[ "${{CACHED_SHA256,,}}" != "${{EXPECTED_SHA256,,}}" ]]; then
        rm -f "$CACHE_ARCHIVE"
    fi
fi

if [[ ! -f "$CACHE_ARCHIVE" ]]; then
    TMP_ARCHIVE="$TMP_DIR/$ARCHIVE_BASENAME"
    curl -fsSL "$MIRROR_URL/$INSTALLER_PROVIDER/$INSTALLER_VERSION/files/$ENCODED_FILE" -o "$TMP_ARCHIVE"
    ACTUAL_SHA256="$(calc_sha256 "$TMP_ARCHIVE")"
    if [[ "${{ACTUAL_SHA256,,}}" != "${{EXPECTED_SHA256,,}}" ]]; then
        echo "Checksum mismatch: expected $EXPECTED_SHA256, got $ACTUAL_SHA256" >&2
        exit 1
    fi
    mv "$TMP_ARCHIVE" "$CACHE_ARCHIVE"
fi

TMP_BIN="$(extract_installer "$CACHE_ARCHIVE" "$TMP_DIR")"
chmod +x "$TMP_BIN" 2>/dev/null || true

COMMAND_ARGS=("--mirror-url" "$MIRROR_URL" "{command}")
{provider_line}
COMMAND_ARGS+=("${{FORWARD_ARGS[@]}}")

exec "$TMP_BIN" "${{COMMAND_ARGS[@]}}"
"#,
        mirror_url = mirror_url,
        installer_provider = installer_provider,
        installer_bin = installer_bin,
        command = command.as_cli(),
        provider_line = provider_line,
    )
}

fn render_ps1(
    command: ScriptCommand,
    provider: &str,
    mirror_url: &str,
    installer_provider: &str,
    installer_bin: &str,
) -> String {
    let provider_line = if command.needs_provider() {
        format!("$CommandArgs += \"{}\"", provider)
    } else {
        String::new()
    };

    format!(
        r#"Param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ArgsList
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$MirrorUrl = if ($env:MIRROR_URL) {{ $env:MIRROR_URL }} else {{ "{mirror_url}" }}
$InstallerProvider = if ($env:INSTALLER_PROVIDER) {{ $env:INSTALLER_PROVIDER }} else {{ "{installer_provider}" }}
$InstallerTag = if ($env:INSTALLER_TAG) {{ $env:INSTALLER_TAG }} else {{ "latest" }}
$InstallerVersion = if ($env:INSTALLER_VERSION) {{ $env:INSTALLER_VERSION }} else {{ "" }}
$InstallerBin = "{installer_bin}"
$CacheRoot = if ($env:ACM_INSTALLER_CACHE_DIR) {{
    $env:ACM_INSTALLER_CACHE_DIR
}} elseif ($env:LOCALAPPDATA) {{
    Join-Path $env:LOCALAPPDATA "acm-installer\\cache"
}} elseif ($env:XDG_CACHE_HOME) {{
    Join-Path $env:XDG_CACHE_HOME "acm-installer"
}} else {{
    Join-Path (Join-Path $HOME ".cache") "acm-installer"
}}
$ForwardArgs = New-Object System.Collections.Generic.List[string]

for ($i = 0; $i -lt $ArgsList.Count; $i++) {{
    switch ($ArgsList[$i]) {{
        "--mirror-url" {{
            $MirrorUrl = $ArgsList[++$i]
        }}
        "--installer-tag" {{
            $InstallerTag = $ArgsList[++$i]
        }}
        "--installer-version" {{
            $InstallerVersion = $ArgsList[++$i]
        }}
        default {{
            [void]$ForwardArgs.Add($ArgsList[$i])
        }}
    }}
}}

if (-not $MirrorUrl) {{
    throw "MIRROR_URL is required"
}}

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLower()
switch ($arch) {{
    "x64" {{ $Platform = "x86_64-pc-windows-msvc" }}
    "arm64" {{ $Platform = "aarch64-pc-windows-msvc" }}
    default {{ throw "Unsupported architecture: $arch" }}
}}

if (-not $InstallerVersion) {{
    $InstallerVersion = (Invoke-RestMethod "$MirrorUrl/$InstallerProvider/$InstallerTag").ToString().Trim()
}}

$Checksums = Invoke-RestMethod "$MirrorUrl/api/$InstallerProvider/checksums"
$VersionNode = $Checksums.$InstallerVersion
if (-not $VersionNode) {{ throw "installer version not found in checksums" }}

$Entry = $VersionNode.$Platform
if (-not $Entry) {{ $Entry = $VersionNode.universal }}
if (-not $Entry) {{
    $FirstProp = $VersionNode.PSObject.Properties | Select-Object -First 1
    if ($FirstProp) {{ $Entry = $FirstProp.Value }}
}}
if (-not $Entry) {{ throw "no installer entry for platform" }}

if ($Entry.files -and $Entry.files.PSObject.Properties.Count -gt 0) {{
    $FileProp = $Entry.files.PSObject.Properties | Sort-Object Name | Select-Object -First 1
    $InstallerFile = $FileProp.Name
    $ExpectedSha256 = $FileProp.Value.sha256
}} else {{
    $InstallerFile = $Entry.filename
    $ExpectedSha256 = $Entry.sha256
}}
if (-not $InstallerFile -or -not $ExpectedSha256) {{ throw "installer filename or sha256 missing" }}

function Encode-Path([string]$PathValue) {{
    return (($PathValue -split '/') | ForEach-Object {{ [System.Uri]::EscapeDataString($_) }}) -join '/'
}}

$TempDir = Join-Path $env:TEMP ("acm-installer-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null
$ArchiveName = [System.IO.Path]::GetFileName($InstallerFile)
$CacheArchiveDir = Join-Path $CacheRoot (Join-Path $InstallerProvider (Join-Path $InstallerVersion $Platform))
$CacheArchivePath = Join-Path $CacheArchiveDir $ArchiveName
New-Item -ItemType Directory -Path $CacheArchiveDir -Force | Out-Null

try {{
    $EncodedFile = Encode-Path $InstallerFile
    if (Test-Path $CacheArchivePath) {{
        $CachedSha256 = (Get-FileHash $CacheArchivePath -Algorithm SHA256).Hash.ToLower()
        if ($CachedSha256 -ne $ExpectedSha256.ToLower()) {{
            Remove-Item -Path $CacheArchivePath -Force -ErrorAction SilentlyContinue
        }}
    }}

    if (-not (Test-Path $CacheArchivePath)) {{
        $ArchivePath = Join-Path $TempDir $ArchiveName
        Invoke-WebRequest -Uri "$MirrorUrl/$InstallerProvider/$InstallerVersion/files/$EncodedFile" -OutFile $ArchivePath
        $ActualSha256 = (Get-FileHash $ArchivePath -Algorithm SHA256).Hash.ToLower()
        if ($ActualSha256 -ne $ExpectedSha256.ToLower()) {{
            throw "Checksum mismatch: expected $ExpectedSha256, got $ActualSha256"
        }}
        Move-Item -Path $ArchivePath -Destination $CacheArchivePath -Force
    }}

    $InstallerPath = $CacheArchivePath
    if ($CacheArchivePath.ToLower().EndsWith(".zip")) {{
        Expand-Archive -Path $CacheArchivePath -DestinationPath $TempDir -Force
        $InstallerPath = Get-ChildItem -Path $TempDir -Recurse -File |
            Where-Object {{ $_.Name -eq ("$InstallerBin.exe") -or $_.Name -eq $InstallerBin }} |
            Select-Object -First 1
        if (-not $InstallerPath) {{ throw "Installer binary not found after extraction" }}
        $InstallerPath = $InstallerPath.FullName
    }} elseif (-not $CacheArchivePath.ToLower().EndsWith(".exe")) {{
        throw "Unsupported installer archive format on PowerShell path: $CacheArchivePath"
    }}

    $CommandArgs = @("--mirror-url", $MirrorUrl, "{command}")
    {provider_line}
    $CommandArgs += $ForwardArgs

    & $InstallerPath @CommandArgs
    exit $LASTEXITCODE
}} finally {{
    if (Test-Path $TempDir) {{
        Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue
    }}
}}
"#,
        mirror_url = mirror_url,
        installer_provider = installer_provider,
        installer_bin = installer_bin,
        command = command.as_cli(),
        provider_line = provider_line,
    )
}

#[cfg(test)]
mod tests {
    use super::{ScriptFlavor, ScriptResolutionContext, resolve_script_variant};

    #[test]
    fn resolve_script_variant_prefers_accept_when_present() {
        let flavor = resolve_script_variant(ScriptResolutionContext {
            accept: Some("application/x-powershell"),
            user_agent: Some("curl/8.0"),
            provider: Some("tool-a"),
        });
        assert_eq!(flavor, ScriptFlavor::Ps1);
    }

    #[test]
    fn resolve_script_variant_uses_accept_quality_weight() {
        let flavor = resolve_script_variant(ScriptResolutionContext {
            accept: Some(
                "text/x-shellscript;q=0.2, application/x-powershell;q=0.9, application/json;q=1.0",
            ),
            user_agent: Some("curl/8.0"),
            provider: Some("tool-a"),
        });
        assert_eq!(flavor, ScriptFlavor::Ps1);
    }

    #[test]
    fn resolve_script_variant_uses_ua_fallback() {
        let flavor = resolve_script_variant(ScriptResolutionContext {
            accept: None,
            user_agent: Some("Mozilla/5.0 (Windows NT) PowerShell/7.4"),
            provider: Some("tool-a"),
        });
        assert_eq!(flavor, ScriptFlavor::Ps1);
    }

    #[test]
    fn resolve_script_variant_defaults_to_sh() {
        let flavor = resolve_script_variant(ScriptResolutionContext {
            accept: None,
            user_agent: Some("curl/8.0"),
            provider: Some("tool-a"),
        });
        assert_eq!(flavor, ScriptFlavor::Sh);
    }
}
