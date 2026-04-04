#Requires -Version 5.1
$ErrorActionPreference = "Stop"

if (-not $env:MIRROR_URL) {
    throw "MIRROR_URL is required"
}

$InstallRoot = "$env:USERPROFILE\.acm"
$BinDir = "$env:USERPROFILE\.agents\bin"

function Invoke-TuiCheck {
    param(
        [Parameter(Mandatory=$true)][string]$Path
    )

    $proc = $null
    if ($Path.ToLower().EndsWith(".cmd")) {
        $proc = Start-Process -FilePath "cmd.exe" -ArgumentList "/c", "`"$Path`"" -PassThru
    } else {
        $proc = Start-Process -FilePath $Path -PassThru
    }

    Start-Sleep -Seconds 2
    if ($proc.HasExited) {
        throw "TUI check failed: $Path exited with $($proc.ExitCode)"
    }
    Stop-Process -Id $proc.Id -Force
}

function Run-Cli {
    param(
        [Parameter(Mandatory=$true)][string]$Name,
        [Parameter(Mandatory=$true)][string]$Bin,
        [string[]]$UninstallArgs = @()
    )

    Write-Host "==> Installing $Name"
    $installScript = Join-Path $env:TEMP ("acm-install-" + $Name + "-" + [guid]::NewGuid().ToString("N") + ".ps1")
    Invoke-WebRequest -Uri "$env:MIRROR_URL/$Name/install.ps1" -UseBasicParsing -OutFile $installScript
    try {
        & $installScript
    } finally {
        Remove-Item -Force -ErrorAction SilentlyContinue $installScript
    }

    $resolved = Get-Command $Name -ErrorAction Stop
    if ($resolved.Source -ne $Bin) {
        throw "PATH check failed: $Name resolved to $($resolved.Source), expected $Bin"
    }

    if ($Name -eq "codex") {
        Write-Host "==> Help check: $Name"
        & $Name --help | Out-Null
    } else {
        Write-Host "==> Version check: $Name"
        & $Name --version

        Write-Host "==> TUI check: $Bin"
        Invoke-TuiCheck $Bin
    }

    $statePath = Join-Path $InstallRoot "state.toml"
    if (-not (Test-Path $statePath)) {
        throw "State file missing: $statePath"
    }

    Write-Host "==> Uninstalling $Name"
    $procName = [IO.Path]::GetFileNameWithoutExtension($Bin)
    Get-Process -Name $procName -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 1
    $uninstallScript = Join-Path $env:TEMP ("acm-uninstall-" + $Name + "-" + [guid]::NewGuid().ToString("N") + ".ps1")
    Invoke-WebRequest -Uri "$env:MIRROR_URL/$Name/uninstall.ps1" -UseBasicParsing -OutFile $uninstallScript
    try {
        & $uninstallScript @UninstallArgs
    } finally {
        Remove-Item -Force -ErrorAction SilentlyContinue $uninstallScript
    }

    for ($i = 0; $i -lt 5; $i++) {
        if (-not (Test-Path $Bin)) { break }
        Start-Sleep -Seconds 1
    }
    if (Test-Path $Bin) {
        throw "Uninstall check failed: $Bin still exists"
    }
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -like "*$BinDir*") {
        throw "Uninstall check failed: $BinDir still in user PATH"
    }
}

if ($env:SKIP_CLAUDE -eq "1") {
    Write-Host "Skipping claude: SKIP_CLAUDE=1"
} else {
    Run-Cli -Name "claude" -Bin "$BinDir\claude.exe"
}
Run-Cli -Name "codex" -Bin "$BinDir\codex.exe"
if ($env:SKIP_GEMINI -eq "1") {
    Write-Host "Skipping gemini: SKIP_GEMINI=1"
} else {
    Run-Cli -Name "gemini" -Bin "$BinDir\gemini.cmd"
}
