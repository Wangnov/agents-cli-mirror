#Requires -Version 5.1
$ErrorActionPreference = "Stop"

if (-not $env:MIRROR_URL) {
    throw "MIRROR_URL is required"
}

$InstallDir = "$env:USERPROFILE\.agents"
$BinDir = "$InstallDir\bin"

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
        & $installScript @("--install-dir", $InstallDir, "--no-modify-path")
    } finally {
        Remove-Item -Force -ErrorAction SilentlyContinue $installScript
    }

    Write-Host "==> Version check: $Bin"
    & $Bin --version

    if ($Name -eq "codex") {
        Write-Host "==> Help check: $Bin"
        & $Bin --help | Out-Null
    } else {
        Write-Host "==> TUI check: $Bin"
        Invoke-TuiCheck $Bin
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
}

if ($env:SKIP_CLAUDE -eq "1") {
    Write-Host "Skipping claude-code: SKIP_CLAUDE=1"
} else {
    Run-Cli -Name "claude-code" -Bin "$BinDir\claude-code.exe"
}
Run-Cli -Name "codex" -Bin "$BinDir\codex.exe"
if ($env:SKIP_GEMINI -eq "1") {
    Write-Host "Skipping gemini: SKIP_GEMINI=1"
} else {
    Run-Cli -Name "gemini" -Bin "$BinDir\gemini.cmd" -UninstallArgs @("-Yes")
}
