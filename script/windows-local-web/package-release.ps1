[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$BuildDirectory,

    [Parameter(Mandatory)]
    [ValidatePattern('\A[0-9a-f]{40}\z')]
    [string]$SourceCommit
)

$ErrorActionPreference = 'Stop'
$build = (Resolve-Path -LiteralPath $BuildDirectory).Path
$web = Join-Path $build 'web'
$remoteServer = Join-Path $build 'cargo/release-fast/remote-server.exe'

foreach ($path in @(
    $remoteServer
    (Join-Path $web 'index.html')
    (Join-Path $web 'zed_web_workspace.js')
    (Join-Path $web 'zed_web_workspace_bg.wasm')
    (Join-Path $web 'zed_web_workspace_bg.wasm.br')
    (Join-Path $web 'bundle.version')
    (Join-Path $web 'profile.version')
)) {
    $file = Get-Item -LiteralPath $path
    if ($file.PSIsContainer -or $file.Length -eq 0) { throw "Expected a nonempty build output at $path." }
}

$release = (New-Item -ItemType Directory -Path (Join-Path $build 'release')).FullName
$webArchive = Join-Path $release 'zed-web.zip'
$remoteExecutable = Join-Path $release 'remote-server.exe'
[IO.Compression.ZipFile]::CreateFromDirectory($web, $webArchive)
Copy-Item -LiteralPath $remoteServer -Destination $remoteExecutable

$artifacts = foreach ($path in $webArchive, $remoteExecutable) {
    $file = Get-Item -LiteralPath $path
    [ordered]@{
        name = $file.Name
        size = $file.Length
        sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

[ordered]@{
    schemaVersion = 1
    sourceCommit = $SourceCommit
    artifacts = @($artifacts)
} | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $release 'manifest.json') -Encoding utf8NoBOM
