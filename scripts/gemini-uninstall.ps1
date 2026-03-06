#Requires -Version 5.1
param(
    [switch]$Yes,
    [switch]$RemoveNodePty,
    [switch]$RemoveNode,
    [switch]$NoNodePty,
    [switch]$NoNode
)

$ErrorActionPreference = "Stop"

$InstallDir = "$env:USERPROFILE\.agents"
$BinDir = "$InstallDir\bin"
$VersionFile = "$InstallDir\versions.json"

$LangCode = if ((Get-UICulture).Name -like "zh*") { "zh" } else { "en" }

function Msg {
    param([string]$Key, [string]$Arg1 = "")

    $messages = @{
        "zh" = @{
            "uninstalling"    = "正在卸载 Gemini CLI..."
            "removed"         = "已删除: $Arg1"
            "remove_node_pty" = "是否同时卸载 node-pty 预编译? [y/N]"
            "remove_node"     = "是否同时卸载私有 Node.js? [y/N]"
            "complete"        = "卸载完成!"
        }
        "en" = @{
            "uninstalling"    = "Uninstalling Gemini CLI..."
            "removed"         = "Removed: $Arg1"
            "remove_node_pty" = "Remove node-pty prebuilds as well? [y/N]"
            "remove_node"     = "Remove private Node.js as well? [y/N]"
            "complete"        = "Uninstallation complete!"
        }
    }

    Write-Host $messages[$LangCode][$Key]
}

function Remove-VersionKey {
    param([string]$Key)
    if (-not (Test-Path $VersionFile)) {
        return
    }
    try {
        $info = Get-Content $VersionFile | ConvertFrom-Json
        $info.PSObject.Properties.Remove($Key)
        $info | ConvertTo-Json -Depth 6 | Set-Content $VersionFile
    } catch {
        # Ignore parse errors
    }
}

Msg "uninstalling"

# Remove Gemini files
if (Test-Path "$BinDir\gemini.cmd") {
    Remove-Item "$BinDir\gemini.cmd" -Force
    Msg "removed" "$BinDir\gemini.cmd"
}

if (Test-Path "$InstallDir\gemini") {
    Remove-Item "$InstallDir\gemini" -Recurse -Force
    Msg "removed" "$InstallDir\gemini"
}

Remove-VersionKey "gemini"

$removeNodePtyDecision = $null
if ($Yes -or $RemoveNodePty) { $removeNodePtyDecision = $true }
if ($NoNodePty) { $removeNodePtyDecision = $false }
if ($null -eq $removeNodePtyDecision) {
    $answer = Read-Host (Msg "remove_node_pty")
    $removeNodePtyDecision = $answer -match '^[Yy]'
}
if ($removeNodePtyDecision) {
    if (Test-Path "$InstallDir\node-pty") {
        Remove-Item "$InstallDir\node-pty" -Recurse -Force
        Msg "removed" "$InstallDir\node-pty"
    }
    Remove-VersionKey "node_pty"
}

$removeNodeDecision = $null
if ($Yes -or $RemoveNode) { $removeNodeDecision = $true }
if ($NoNode) { $removeNodeDecision = $false }
if ($null -eq $removeNodeDecision) {
    $answer = Read-Host (Msg "remove_node")
    $removeNodeDecision = $answer -match '^[Yy]'
}
if ($removeNodeDecision) {
    if (Test-Path "$InstallDir\node") {
        Remove-Item "$InstallDir\node" -Recurse -Force
        Msg "removed" "$InstallDir\node"
    }
    Remove-VersionKey "node"
}

Msg "complete"
