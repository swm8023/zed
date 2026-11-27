[CmdletBinding()]
param(
    [string]$WasiSdk = $env:WASI_SDK_PATH,
    [ValidateSet('All', 'Web', 'RemoteServer')]
    [string]$Component = 'All',
    [string]$WasmBindgen = 'wasm-bindgen',
    [string]$WasmToolchain = 'stable',
    [string]$Node = 'node',
    [string]$OutputDirectory,
    [string[]]$CargoArguments = @()
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repo 'target/windows-local-web'
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$buildDirectory = Join-Path $OutputDirectory 'cargo'
$settings = @(
    '--locked', '--manifest-path', (Join-Path $repo 'Cargo.toml'),
    '--target-dir', $buildDirectory
) + $CargoArguments

Push-Location $repo
try {
    if ($Component -in 'All', 'RemoteServer') {
        cargo build @settings --profile release-fast -p remote_server
        if ($LASTEXITCODE) { throw "Windows remote-server failed to build ($LASTEXITCODE)." }
    }

    if ($Component -in 'All', 'Web') {
        if (-not $WasiSdk) { throw 'Pass -WasiSdk or set WASI_SDK_PATH to build the browser editor.' }
        $lock = Get-Content -LiteralPath (Join-Path $repo 'Cargo.lock') -Raw
        $version = [regex]::Match($lock, '(?m)^name = "wasm-bindgen"\r?\nversion = "([^"]+)"').Groups[1].Value
        $bindgenVersion = & $WasmBindgen --version
        if ($LASTEXITCODE -or $bindgenVersion -ne "wasm-bindgen $version") {
            throw "Install wasm-bindgen-cli $version to match Cargo.lock."
        }
        $sdk = (Resolve-Path -LiteralPath $WasiSdk).Path
        $compiler = Join-Path $sdk 'bin/clang.exe'
        $headers = Join-Path $sdk 'share/wasi-sysroot/include/wasm32-wasip1'
        if (-not (Test-Path $compiler -PathType Leaf) -or -not (Test-Path $headers -PathType Container)) {
            throw "Could not find the WASI SDK compiler and headers at $sdk."
        }

        $env:CC_wasm32_unknown_unknown = $compiler
        $env:CFLAGS_wasm32_unknown_unknown = "--target=wasm32-wasip1 -matomics -mbulk-memory -isystem `"$($headers.Replace('\', '/'))`""
        $env:RUSTC_BOOTSTRAP = '1'
        $env:CARGO_PROFILE_RELEASE_FAST_PANIC = 'abort'
        $env:CARGO_PROFILE_RELEASE_FAST_DEBUG = '0'
        $env:CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS = @(
            '--cfg getrandom_backend="wasm_js"'
            '-C target-feature=+atomics,+bulk-memory,+mutable-globals'
            '-C link-arg=--shared-memory'
            '-C link-arg=--import-memory'
            '-C link-arg=--max-memory=1073741824'
            '-C link-arg=--export=__wasm_init_tls'
            '-C link-arg=--export=__tls_size'
            '-C link-arg=--export=__tls_align'
            '-C link-arg=--export=__tls_base'
        ) -join ' '

        $webProfile = @(
            '--config', 'profile.release-fast.opt-level="z"'
            '--config', 'profile.release-fast.lto="thin"'
            '--config', 'profile.release-fast.codegen-units=1'
        )
        cargo "+$WasmToolchain" rustc @settings @webProfile -Z build-std=std,panic_abort --profile release-fast --target wasm32-unknown-unknown -p zed_web_workspace -- -C lto=fat
        if ($LASTEXITCODE) { throw "Browser editor failed to build ($LASTEXITCODE)." }
        $web = Join-Path $OutputDirectory 'web'
        New-Item -ItemType Directory -Force -Path $web | Out-Null
        $module = Join-Path $buildDirectory 'wasm32-unknown-unknown/release-fast/zed_web_workspace.wasm'
        & $WasmBindgen --target web --no-typescript --remove-name-section --remove-producers-section --out-dir $web $module
        if ($LASTEXITCODE) { throw "wasm-bindgen failed ($LASTEXITCODE)." }
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'index.html') -Destination (Join-Path $web 'index.html')
        $builtinThemes = Join-Path $web 'builtin-themes'
        New-Item -ItemType Directory -Force -Path $builtinThemes | Out-Null
        foreach ($family in 'ayu', 'gruvbox', 'one') {
            $themeOutput = Join-Path $builtinThemes $family
            New-Item -ItemType Directory -Force -Path $themeOutput | Out-Null
            Copy-Item -LiteralPath (Join-Path $repo "assets/themes/$family/LICENSE") -Destination $themeOutput
            if ($family -ne 'one') {
                Copy-Item -LiteralPath (Join-Path $repo "assets/themes/$family/$family.json") -Destination $themeOutput
            }
        }
        Copy-Item -LiteralPath (Join-Path $repo 'assets/themes/LICENSES') -Destination $builtinThemes
        & $Node (Join-Path $PSScriptRoot 'pack-web.mjs') $web
        if ($LASTEXITCODE) { throw "Could not compress the Web bundle ($LASTEXITCODE)." }
        Set-Content -LiteralPath (Join-Path $web 'profile.version') -Value '1' -Encoding ascii
        Get-Item (Join-Path $web 'zed_web_workspace_bg.wasm') | Select-Object FullName, Length
    }
} finally {
    Pop-Location
}
