Param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ArgsList
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Provider = "codex"
$DefaultMirrorUrl = "https://install.agentsmirror.com"
$MirrorUrl = if ($env:MIRROR_URL) { $env:MIRROR_URL } else { $DefaultMirrorUrl }
$UserHome = if ($env:USERPROFILE) { $env:USERPROFILE } elseif ($HOME) { $HOME } else { [System.IO.Path]::GetTempPath() }
$LocalAppData = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { Join-Path $UserHome "AppData\Local" }
$InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { Join-Path $LocalAppData "Programs\codex" }
$VersionPin = ""

function Fail {
    param([string]$Message)
    [Console]::Error.WriteLine("Error: $Message")
    exit 1
}

function Usage {
    @"
Usage: codex.ps1 [options]
  --mirror <url>       mirror base URL (default: https://install.agentsmirror.com)
  --version <tag>      install a pinned version manifest instead of latest
  --install-dir <dir>  install directory (default: %LOCALAPPDATA%\Programs\codex)
  -h, --help           show this help

Environment:
  MIRROR_URL           mirror base URL override
  INSTALL_DIR          install directory override
"@
}

function Require-Value {
    param([int]$Index, [string]$Name)
    if (($Index + 1) -ge $ArgsList.Count) {
        Fail "$Name requires a value"
    }
}

for ($i = 0; $i -lt $ArgsList.Count; $i++) {
    switch ($ArgsList[$i]) {
        "--mirror" {
            Require-Value $i $ArgsList[$i]
            $i++
            $MirrorUrl = $ArgsList[$i]
        }
        "--mirror-url" {
            Require-Value $i $ArgsList[$i]
            $i++
            $MirrorUrl = $ArgsList[$i]
        }
        "--version" {
            Require-Value $i $ArgsList[$i]
            $i++
            $VersionPin = $ArgsList[$i]
        }
        "--install-dir" {
            Require-Value $i $ArgsList[$i]
            $i++
            $InstallDir = $ArgsList[$i]
        }
        "-h" {
            Usage
            exit 0
        }
        "--help" {
            Usage
            exit 0
        }
        Default {
            Fail "Unknown option: $($ArgsList[$i])"
        }
    }
}

if (-not $MirrorUrl) {
    Fail "MIRROR_URL is empty"
}
$MirrorUrl = $MirrorUrl.TrimEnd("/")

function Get-PlatformKey {
    $Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if (-not $Arch) {
        Fail "Unable to detect Windows architecture"
    }

    switch ($Arch.ToLowerInvariant()) {
        "amd64" { return "x86_64-pc-windows-msvc" }
        "x64" { return "x86_64-pc-windows-msvc" }
        "arm64" { return "aarch64-pc-windows-msvc" }
        "aarch64" { return "aarch64-pc-windows-msvc" }
        Default { Fail "Unsupported Windows architecture: $Arch" }
    }
}

function Download-File {
    param([string]$Uri, [string]$OutFile)

    $Params = @{
        Uri = $Uri
        OutFile = $OutFile
    }
    if ($PSVersionTable.PSVersion.Major -lt 6) {
        $Params.UseBasicParsing = $true
    }
    try {
        Invoke-WebRequest @Params | Out-Null
    } catch {
        Fail "Failed to download $Uri. $($_.Exception.Message)"
    }
}

function Read-JsonFile {
    param([string]$Path)

    try {
        return Get-Content -Path $Path -Raw | ConvertFrom-Json
    } catch {
        Fail "Failed to parse manifest JSON. $($_.Exception.Message)"
    }
}

function Normalize-Dir {
    param([string]$Path)

    $TrimChars = [char[]]@("\", "/")
    try {
        return ([System.IO.Path]::GetFullPath($Path)).TrimEnd($TrimChars).ToLowerInvariant()
    } catch {
        return $Path.TrimEnd($TrimChars).ToLowerInvariant()
    }
}

function Test-DirOnPath {
    param([string]$Dir)

    $Target = Normalize-Dir $Dir
    foreach ($Part in ($env:Path -split ";")) {
        if ($Part -and ((Normalize-Dir $Part) -eq $Target)) {
            return $true
        }
    }
    return $false
}

$PlatformKey = Get-PlatformKey
$ManifestName = if ($VersionPin) { $VersionPin } else { "latest" }
$ManifestUrl = "$MirrorUrl/$Provider/$ManifestName.json"
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) "agents-$Provider-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

try {
    $ManifestPath = Join-Path $TempDir "latest.json"
    Download-File $ManifestUrl $ManifestPath
    $Manifest = Read-JsonFile $ManifestPath

    if ($Manifest.provider -ne $Provider) {
        Fail "Manifest provider is '$($Manifest.provider)', expected '$Provider'"
    }
    if (-not $Manifest.version) {
        Fail "Manifest is missing version"
    }

    $EntryProperty = $Manifest.platforms.PSObject.Properties[$PlatformKey]
    if (-not $EntryProperty) {
        Fail "Mirror manifest does not support platform '$PlatformKey' for provider '$Provider'"
    }
    $Entry = $EntryProperty.Value
    if (-not $Entry.file -or -not $Entry.sha256 -or -not $Entry.bin) {
        Fail "Manifest entry for '$PlatformKey' is missing file, sha256, or bin"
    }
    if ($Entry.file -match "[/\\]") {
        Fail "Manifest artifact file is invalid: $($Entry.file)"
    }

    $ArtifactUrl = "$MirrorUrl/$Provider/$($Manifest.version)/$PlatformKey/$($Entry.file)"
    $ArtifactPath = Join-Path $TempDir $Entry.file
    Download-File $ArtifactUrl $ArtifactPath

    $Expected = $Entry.sha256.ToLowerInvariant()
    $Actual = (Get-FileHash -Path $ArtifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Actual -ne $Expected) {
        Fail "SHA256 mismatch for $($Entry.file): expected $Expected, got $Actual"
    }

    $BinPath = $ArtifactPath
    if ($Entry.file.ToLowerInvariant().EndsWith(".zip")) {
        $ExtractDir = Join-Path $TempDir "extract"
        New-Item -ItemType Directory -Force -Path $ExtractDir | Out-Null
        Expand-Archive -Path $ArtifactPath -DestinationPath $ExtractDir -Force
        $BinaryItem = Get-ChildItem -Path $ExtractDir -Recurse | Where-Object {
            -not $_.PSIsContainer -and $_.Name -eq $Entry.bin
        } | Select-Object -First 1
        if (-not $BinaryItem) {
            Fail "Binary '$($Entry.bin)' was not found after extraction"
        }
        $BinPath = $BinaryItem.FullName
    } elseif ($Entry.file.ToLowerInvariant().EndsWith(".tar.gz") -or $Entry.file.ToLowerInvariant().EndsWith(".tgz")) {
        Fail "Windows installer cannot extract tar.gz artifact '$($Entry.file)'"
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $InstallPath = Join-Path $InstallDir $Entry.bin
    Copy-Item -Path $BinPath -Destination $InstallPath -Force

    Write-Host "Success: installed $Provider $($Manifest.version) to $InstallPath"
    if (-not (Test-DirOnPath $InstallDir)) {
        $EscapedInstallDir = $InstallDir.Replace("'", "''")
        Write-Host "$InstallDir is not on PATH."
        Write-Host "Add it to PATH with:"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$EscapedInstallDir', 'User')"
    }
} finally {
    if (Test-Path $TempDir) {
        Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
