#Requires -Version 5.1
$ErrorActionPreference = "Stop"

$InstallDir = "$env:USERPROFILE\.agents"
$BinDir = "$InstallDir\bin"

Write-Host "Uninstalling CLI tools..."

# Check if any tools are running
$processes = @("claude", "codex", "gemini") | ForEach-Object {
    Get-Process -Name $_ -ErrorAction SilentlyContinue
} | Where-Object { $_ }

if ($processes) {
    Write-Host "The following processes are running:"
    $processes | ForEach-Object { Write-Host "  - $($_.Name) (PID: $($_.Id))" }
    $choice = Read-Host "Do you want to stop them? (y/N)"
    if ($choice -eq 'y') {
        $processes | Stop-Process -Force
        Start-Sleep -Seconds 1
    } else {
        Write-Host "Please close these applications first."
        exit 1
    }
}

# Remove installation directory
if (Test-Path $InstallDir) {
    Remove-Item -Recurse -Force $InstallDir
    Write-Host "Removed $InstallDir"
}

# Remove from PATH
$CurrentPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($CurrentPath -like "*$BinDir*") {
    $NewPath = ($CurrentPath -split ';' | Where-Object { $_ -ne $BinDir }) -join ';'
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    Write-Host "Removed $BinDir from PATH"
}

Write-Host "Uninstallation complete!"
Write-Host "Please restart your terminal."
