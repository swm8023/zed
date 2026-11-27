[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Workspace,
    [string]$OutputDirectory,
    [ValidateRange(1024, 65535)]
    [int]$Port = 48680,
    [string]$Python = 'python',
    [string]$StateDirectory,
    [string]$GlobalConfig,
    [string]$PublicOrigin,
    [ValidateSet('127.0.0.1', '0.0.0.0')]
    [string]$ListenAddress = '127.0.0.1',
    [string]$TlsCertificate,
    [string]$TlsKey
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$workspacePath = (Resolve-Path -LiteralPath $Workspace).Path
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repo 'target/windows-local-web'
}
if (-not $StateDirectory) {
    $StateDirectory = Join-Path ([IO.Path]::GetTempPath()) "zed-web-$Port"
}
$web = Join-Path $OutputDirectory 'web'
$server = Join-Path $OutputDirectory 'cargo/release-fast/remote-server.exe'
if (-not (Test-Path (Join-Path $web 'zed_web_workspace_bg.wasm') -PathType Leaf) -or
    -not (Test-Path $server -PathType Leaf)) {
    throw 'Run build.ps1 to create the browser and Windows remote-server artifacts.'
}
$arguments = @('--workspace', $workspacePath, '--remote-server', $server, '--web', $web,
               '--state', $StateDirectory, '--port', $Port, '--listen', $ListenAddress)
if ($GlobalConfig) {
    $arguments += @('--global-config', (Resolve-Path -LiteralPath $GlobalConfig).Path)
}
if ($PublicOrigin) {
    $arguments += @('--public-origin', $PublicOrigin)
}
if ($TlsCertificate) {
    $arguments += @('--tls-certfile', (Resolve-Path -LiteralPath $TlsCertificate).Path)
}
if ($TlsKey) {
    $arguments += @('--tls-keyfile', (Resolve-Path -LiteralPath $TlsKey).Path)
}
& $Python -B (Join-Path $PSScriptRoot 'gateway.py') @arguments
exit $LASTEXITCODE
