#Requires -Version 7.0
<#
.SYNOPSIS
Updates the current Scoop Zed installation from a local build and reopens its local project directories.

.PARAMETER BuildDirectory
Directory containing zed.exe and cli.exe (or an extracted bundle containing bin\zed.exe).

.PARAMETER ProjectPath
Directories to reopen instead of reading the current release channel's Zed session.
.PARAMETER ScoopRoot
Override the Scoop installation root, for example for a global installation.
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$BuildDirectory = (Join-Path $PSScriptRoot '../target/release-fast'),
    [string[]]$ProjectPath,
    [string]$ScoopRoot,
    [ValidateRange(10, 600)][int]$CloseTimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if (-not ('ZedScoopUpdate.NativeMethods' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace ZedScoopUpdate {
    public static class NativeMethods {
        private delegate bool WindowCallback(IntPtr window, IntPtr state);
        private delegate int SqliteCallback(IntPtr state, int count, IntPtr values, IntPtr names);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool EnumWindows(WindowCallback callback, IntPtr state);
        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr window);
        [DllImport("user32.dll")]
        private static extern IntPtr GetWindow(IntPtr window, uint command);
        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr window, out int processId);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool PostMessageW(IntPtr window, uint message, IntPtr parameter, IntPtr data);

        [DllImport("winsqlite3.dll", CallingConvention = CallingConvention.Cdecl)]
        private static extern int sqlite3_open_v2([MarshalAs(UnmanagedType.LPUTF8Str)] string path, out IntPtr db, int flags, IntPtr vfs);
        [DllImport("winsqlite3.dll", CallingConvention = CallingConvention.Cdecl)]
        private static extern int sqlite3_exec(IntPtr db, [MarshalAs(UnmanagedType.LPUTF8Str)] string sql, SqliteCallback callback, IntPtr data, IntPtr error);
        [DllImport("winsqlite3.dll", CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr sqlite3_errmsg(IntPtr db);
        [DllImport("winsqlite3.dll", CallingConvention = CallingConvention.Cdecl)]
        private static extern int sqlite3_close(IntPtr db);

        public static IntPtr[] VisibleWindows(int[] processIds) {
            var owners = new HashSet<int>(processIds);
            var windows = new List<IntPtr>();
            if (!EnumWindows((window, state) => {
                GetWindowThreadProcessId(window, out int owner);
                if (owners.Contains(owner) && IsWindowVisible(window) && GetWindow(window, 4) == IntPtr.Zero)
                    windows.Add(window);
                return true;
            }, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            return windows.ToArray();
        }

        public static void CloseWindow(IntPtr window, int[] processIds) {
            GetWindowThreadProcessId(window, out int owner);
            if (Array.IndexOf(processIds, owner) >= 0 &&
                !PostMessageW(window, 0x0010, IntPtr.Zero, IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error());
        }

        public static string[] Query(string path, string sql) {
            var values = new List<string>();
            int result = sqlite3_open_v2(path, out IntPtr db, 1, IntPtr.Zero);
            try {
                if (result == 0)
                    result = sqlite3_exec(db, sql, (state, count, row, names) => {
                        values.Add(Marshal.PtrToStringUTF8(Marshal.ReadIntPtr(row)) ?? "");
                        return 0;
                    }, IntPtr.Zero, IntPtr.Zero);
                if (result != 0)
                    throw new InvalidOperationException(Marshal.PtrToStringUTF8(sqlite3_errmsg(db)));
                return values.ToArray();
            } finally {
                if (db != IntPtr.Zero)
                    sqlite3_close(db);
            }
        }
    }
}
'@
}

function Get-ReleaseChannel([string]$Executable) {
    $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($Executable).ProductVersion
    if ($version -notmatch '\+(dev|stable|preview|nightly)(\.|$)') {
        throw "Cannot determine the Zed release channel of $Executable."
    }
    $Matches[1].ToLowerInvariant()
}

function Get-PeMachine([string]$Executable) {
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Executable))
    try {
        if ($reader.ReadUInt16() -ne 0x5a4d) { throw "Invalid Windows executable $Executable." }
        $reader.BaseStream.Position = 0x3c
        $reader.BaseStream.Position = $reader.ReadInt32()
        if ($reader.ReadUInt32() -ne 0x4550) { throw "Invalid Windows executable $Executable." }
        $reader.ReadUInt16()
    } finally {
        $reader.Dispose()
    }
}

function Get-OpenProjectPaths([string]$Channel) {
    $database = Join-Path $env:LOCALAPPDATA "Zed/db/0-$Channel/db.sqlite"
    if (-not (Test-Path -LiteralPath $database -PathType Leaf)) { return }

    $query = @'
SELECT CASE WHEN remote_connection_id IS NULL THEN COALESCE(paths, '') ELSE char(1) END
FROM workspaces
WHERE session_id = (SELECT value FROM kv_store WHERE key = 'session_id')
ORDER BY timestamp DESC, workspace_id
'@
    $workspaces = [ZedScoopUpdate.NativeMethods]::Query($database, $query)
    if ($workspaces -contains [string][char]1) {
        throw 'The session includes a remote project. Pass -ProjectPath to specify the local directories to reopen.'
    }
    foreach ($path in ($workspaces -split "`n")) {
        if (-not $path) { continue }
        if (Test-Path -LiteralPath $path -PathType Container) {
            $path
        } else {
            Write-Warning "Skipping project directory that cannot be opened: $path"
        }
    }
}

function Get-InstalledProcesses([string[]]$Executables) {
    Get-Process -Name Zed -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -in $Executables }
}

function Start-Zed([string]$Executable, [string[]]$Projects) {
    $launch = [Diagnostics.ProcessStartInfo]::new($Executable)
    foreach ($directory in $Projects) { $launch.ArgumentList.Add($directory) }
    $editor = [Diagnostics.Process]::Start($launch)
    try {
        if ($editor.WaitForExit(3000)) {
            throw "Zed exited immediately with code $($editor.ExitCode)."
        }
    } finally {
        $editor.Dispose()
    }
}

function Close-InstalledZed([string[]]$Executables, [int]$TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $requested = [Collections.Generic.HashSet[IntPtr]]::new()
    $hasClosedWindow = $false
    while ($processes = @(Get-InstalledProcesses $Executables)) {
        if ([DateTime]::UtcNow -ge $deadline) {
            throw 'Zed is still running. Finish any save prompts and try again.'
        }
        $processIds = [int[]]$processes.Id
        $windows = [ZedScoopUpdate.NativeMethods]::VisibleWindows($processIds)
        if (-not $hasClosedWindow -and $windows.Count -eq 0) {
            throw 'No visible Scoop Zed windows were found. Run this script from the Windows desktop that is running Zed.'
        }
        if ($windows.Count -and -not ($windows | Where-Object { $requested.Contains($_) })) {
            [ZedScoopUpdate.NativeMethods]::CloseWindow($windows[0], $processIds)
            $hasClosedWindow = $true
            $null = $requested.Add($windows[0])
        }
        Start-Sleep -Milliseconds 250
    }
}

if (-not $ScoopRoot) {
    $ScoopRoot = $env:SCOOP
    if (-not $ScoopRoot) {
        $scoopCommand = Get-Command scoop.cmd -CommandType Application
        $ScoopRoot = Split-Path -Parent (Split-Path -Parent $scoopCommand.Source)
    }
}

$current = Get-Item -LiteralPath (Join-Path $ScoopRoot 'apps/zed/current')
$installation = $current.ResolveLinkTarget($true)
if (-not $installation -or $installation.Parent.FullName -ne $current.Parent.FullName) {
    throw "Expected a Scoop version junction at $($current.FullName)."
}
$installedEditor = Join-Path $installation.FullName 'Zed.exe'
$installedCli = Join-Path $installation.FullName 'bin/zed.exe'
$build = (Resolve-Path -LiteralPath $BuildDirectory).Path
$builtEditor = Join-Path $build 'zed.exe'
$builtCli = Join-Path $build 'cli.exe'
if (-not (Test-Path -LiteralPath $builtCli -PathType Leaf)) {
    $builtCli = Join-Path $build 'bin/zed.exe'
}

$oldChannel = Get-ReleaseChannel $installedEditor
$newChannel = Get-ReleaseChannel $builtEditor
if ((Get-ReleaseChannel $builtCli) -ne $newChannel) {
    throw 'The new GUI and CLI belong to different release channels.'
}
$machine = Get-PeMachine $installedEditor
if ((Get-PeMachine $builtEditor) -ne $machine -or (Get-PeMachine $builtCli) -ne $machine) {
    throw 'The new GUI and CLI must match the installed Zed architecture.'
}

$files = [ordered]@{'Zed.exe' = $builtEditor; 'bin/zed.exe' = $builtCli}
$architecture = switch ($machine) {
    0x8664 { 'x64' }
    0xaa64 { 'arm64' }
    default { throw 'The current Scoop installation is neither x64 nor ARM64.' }
}
foreach ($relativePath in @('conpty.dll', 'amd_ags_x64.dll', "$architecture/OpenConsole.exe")) {
    $source = Join-Path $build $relativePath
    if ($relativePath -eq "$architecture/OpenConsole.exe" -and
        -not (Test-Path -LiteralPath $source -PathType Leaf)) {
        $source = Join-Path $build 'OpenConsole.exe'
    }
    if ((Test-Path -LiteralPath $source -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $installation.FullName $relativePath) -PathType Leaf)) {
        $files[$relativePath] = $source
    }
}

$updateId = [Guid]::NewGuid().ToString('N')
$lock = [Threading.Mutex]::new($false, 'Local\ZedScoopUpdate')
$locked = $false
try {
    try {
        $locked = $lock.WaitOne(0)
    } catch [Threading.AbandonedMutexException] {
        $locked = $true
    }
    if (-not $locked) { throw 'Another Scoop Zed update is running.' }

    $pending = foreach ($relativePath in $files.Keys) {
        $target = Join-Path $installation.FullName $relativePath
        $sourceHash = (Get-FileHash -LiteralPath $files[$relativePath]).Hash
        if ($sourceHash -ne (Get-FileHash -LiteralPath $target).Hash) {
            [pscustomobject]@{
                Target = $target
                Source = $files[$relativePath]
                RelativePath = $relativePath
                Staged = "$target.$updateId.tmp"
                Hash = $sourceHash
            }
        }
    }
    if (-not $pending) {
        Write-Host "Scoop Zed already matches this build at $($installation.FullName)."
        return
    }

    if ($PSBoundParameters.ContainsKey('ProjectPath')) {
        $projects = @($ProjectPath | ForEach-Object {
            $directory = Get-Item -LiteralPath $_
            if (-not $directory.PSIsContainer) { throw "Expected a project directory at $_." }
            $directory.FullName
        })
    } else {
        $projects = @(Get-OpenProjectPaths $oldChannel)
    }
    $projects = @($projects | Select-Object -Unique)
    foreach ($directory in $projects) { Write-Host "Reopen: $directory" }

    if (-not $PSCmdlet.ShouldProcess($installation.FullName, "Install Zed $newChannel from $build and reopen projects")) {
        return
    }
    if ($oldChannel -ne $newChannel) {
        Write-Warning 'The release channel is changing. Save your edits before updating because each channel has its own workspace database.'
    }

    $backup = Join-Path $ScoopRoot "persist/zed-local-update/$(Get-Date -Format 'yyyyMMdd-HHmmss')-$updateId"
    $executables = @($installedEditor, (Join-Path $current.FullName 'Zed.exe'))
    $replaced = [Collections.Generic.List[object]]::new()
    $wasRunning = [bool]@(Get-InstalledProcesses $executables).Count
    try {
        foreach ($file in $pending) {
            $original = Join-Path $backup $file.RelativePath
            $null = New-Item -ItemType Directory -Path (Split-Path -Parent $original) -Force
            Copy-Item -LiteralPath $file.Target -Destination $original
            Copy-Item -LiteralPath $file.Source -Destination $file.Staged
            if ((Get-FileHash -LiteralPath $file.Staged).Hash -ne $file.Hash) {
                throw "Build file changed while staging $($file.Source)."
            }
        }
        Write-Host 'Closing Zed. Respond to any save prompts in the editor.'
        Close-InstalledZed $executables $CloseTimeoutSeconds
        $activeInstallation = (Get-Item -LiteralPath $current.FullName).ResolveLinkTarget($true)
        if (-not $activeInstallation -or $activeInstallation.FullName -ne $installation.FullName) {
            throw 'Scoop changed the current Zed version during the update. Run the script again.'
        }
        foreach ($file in $pending) {
            [IO.File]::Replace($file.Staged, $file.Target, [NullString]::Value)
            $replaced.Add($file)
        }
        & $installedCli --version
        if ($LASTEXITCODE -ne 0) { throw "The updated CLI exited with code $LASTEXITCODE." }

        Start-Zed $installedEditor $projects
        Write-Host "Scoop Zed updated. Previous files are in $backup."
    } catch {
        $restored = $true
        for ($index = $replaced.Count - 1; $index -ge 0; $index--) {
            $file = $replaced[$index]
            try {
                Copy-Item -LiteralPath (Join-Path $backup $file.RelativePath) -Destination $file.Staged
                [IO.File]::Replace($file.Staged, $file.Target, [NullString]::Value)
            } catch {
                $restored = $false
                Write-Warning "Could not restore $($file.Target) from $backup. $_"
            }
        }
        if ($restored -and $wasRunning -and $replaced.Count) {
            try {
                Start-Zed $installedEditor $projects
                Write-Warning 'Reopened the previous Zed version after rolling back the update.'
            } catch {
                Write-Warning "The previous files were restored, but Zed could not be reopened. $_"
            }
        }
        throw
    } finally {
        foreach ($file in $pending) {
            if (Test-Path -LiteralPath $file.Staged) {
                Remove-Item -LiteralPath $file.Staged -ErrorAction Continue
            }
        }
    }
} finally {
    if ($locked) { $lock.ReleaseMutex() }
    $lock.Dispose()
}
