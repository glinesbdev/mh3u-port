# Runs the game in a window on Windows (Windows PowerShell 5.1 or PowerShell 7), writing what it prints to a log as well.
#   powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1                     # play; output also in .\mh3u_latest.log
#   powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1 -Fps                # with FPS and the [draw times] lines
#   powershell -ExecutionPolicy Bypass -File scripts\win-run.ps1 -Dump frames -Frames 2400,3000
# Build first with scripts\win-build.ps1. See docs/windows.md.
[CmdletBinding()]
param(
    # The folder with the [Game] and [Update] dumps.
    [string]$Games = (Join-Path $env:USERPROFILE 'Downloads\wiiu'),
    # Where win-build.ps1 put the translated C and game.dll.
    [string]$Out = (Join-Path $env:LOCALAPPDATA 'mh3u\c'),
    # Print the FPS and the graphics timings (MH3U_FPS and MH3U_GFX_TIMES).
    [switch]$Fps,
    # Borderless fullscreen on the window's monitor (--fullscreen); F11 or Alt+Enter toggles it while playing.
    [switch]$Fullscreen,
    # Print the graphics report at the end (MH3U_GFX_REPORT).
    [switch]$Report,
    # Write frames (draws, shaders, textures, the picture) to this folder (MH3U_GFX_DUMP_DIR); which ones is -Frames.
    [string]$Dump,
    # The frames -Dump writes, counting swaps from 0 (MH3U_GFX_DUMP_FRAME), e.g. 2400 or 2400,3000.
    [string[]]$Frames = @('2400'),
    # Replace what pixel shaders put out (MH3U_GFX_DEBUG_PS): white, alpha1, tex, uv, inputN, ... (docs/translator.md).
    [string]$DebugPs,
    # Which pad buttons are the game's A/B and X/Y (MH3U_PAD_LAYOUT): xbox (the default) or nintendo.
    [ValidateSet('xbox', 'nintendo')]
    [string]$PadLayout,
    # The log file. By default .\mh3u_latest.log; an older mh3u_latest.log is renamed after the time it was written.
    [string]$Log,
    # The internal resolution scale (MH3U_GFX_RESOLUTION_SCALE): 1, 2, 3, or 4. Unset keeps the game's own size.
    [ValidateSet('1', '2', '3', '4')]
    [string]$Scale,
    # Opt-in: the wishlist's status in the item menu's last empty slot, drawn by the game (MH3U_NATIVE_TEXT; docs/native-menu.md).
    [switch]$NativeText,
    # Opt-in, read-only: log the start menu's state on each change (MH3U_MENU_PROBE; docs/native-menu.md).
    [switch]$MenuProbe,
    # Opt-in: while the mod key is held, the start menu's Gestures row reads "Mods" and A on it logs instead of opening Gestures
    # (MH3U_NATIVE_MENU; docs/native-menu.md). Needs a build translated with the current mods/hooks.txt (win-build.ps1 -Translate).
    [switch]$NativeMenu,
    # The mod key for -NativeMenu, as winit names keys (ShiftRight, AltLeft, KeyM...; MH3U_NATIVE_MENU_KEY). Default ShiftRight.
    [string]$NativeMenuKey
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
# full paths, from where the script was started (they need not exist yet)
$Games = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Games)
$Out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
$tools = Join-Path $repo 'target\release\mh3u-tools.exe'
foreach ($p in @($tools, (Join-Path $Out 'game.dll'), $Games)) {
    if (-not (Test-Path -LiteralPath $p)) {
        throw "not found: $p (build with scripts\win-build.ps1, or pass -Games/-Out)"
    }
}

# Debugging variables left over from an earlier run in this session would change what the game draws or prints.
$stale = @(
    'MH3U_FPS', 'MH3U_GFX_TIMES', 'MH3U_GFX_REPORT', 'MH3U_GFX_LOG', 'MH3U_GFX_DUMP_FRAME', 'MH3U_GFX_DUMP_DIR',
    'MH3U_GFX_DEBUG_PS', 'MH3U_GFX_DEBUG_PS_ONLY', 'MH3U_GFX_SOLID', 'MH3U_GFX_DEPTH_BIAS', 'MH3U_GFX_TEXDUMP',
    'MH3U_GFX_SAVE', 'MH3U_GFX_SAVE_EVERY', 'MH3U_GFX_SAVE_IMAGE', 'MH3U_PAD_LAYOUT', 'MH3U_GFX_RESOLUTION_SCALE', 'MH3U_NATIVE_TEXT', 'MH3U_MENU_PROBE',
    'MH3U_NATIVE_MENU', 'MH3U_NATIVE_MENU_KEY'
)
foreach ($name in $stale) {
    if (Test-Path -LiteralPath "Env:$name") {
        Write-Host "cleared $name (was '$((Get-Item -LiteralPath "Env:$name").Value)')"
        Remove-Item -LiteralPath "Env:$name"
    }
}
$set = [ordered]@{}
if ($Fps) { $set['MH3U_FPS'] = '1'; $set['MH3U_GFX_TIMES'] = '1' }
if ($Report) { $set['MH3U_GFX_REPORT'] = '1' }
if ($Dump) {
    $set['MH3U_GFX_DUMP_DIR'] = $Dump
    $set['MH3U_GFX_DUMP_FRAME'] = ($Frames | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ }) -join ','
}
if ($DebugPs) { $set['MH3U_GFX_DEBUG_PS'] = $DebugPs }
if ($PadLayout) { $set['MH3U_PAD_LAYOUT'] = $PadLayout }

if (-not $Log) {
    $Log = Join-Path (Get-Location).Path 'mh3u_latest.log'
    if (Test-Path -LiteralPath $Log) {
        $old = Get-Item -LiteralPath $Log
        $Rename = 'mh3u_' + $old.LastWriteTime.ToString('yyyy-MM-dd_HHmmss') + '.log'
        Rename-Item -LiteralPath $Log -NewName $Rename
        Write-Host "kept the previous log as $Rename"
    }
} elseif (-not [System.IO.Path]::IsPathRooted($Log)) {
    $Log = Join-Path (Get-Location).Path $Log
}

if ($Scale) {
    $set['MH3U_GFX_RESOLUTION_SCALE'] = $Scale
}
if ($NativeText) {
    $set['MH3U_NATIVE_TEXT'] = '1'
}
if ($MenuProbe) {
    $set['MH3U_MENU_PROBE'] = '1'
}
if ($NativeMenu) {
    $set['MH3U_NATIVE_MENU'] = '1'
}
if ($NativeMenuKey) {
    $set['MH3U_NATIVE_MENU_KEY'] = $NativeMenuKey
}

$runArgs = @('run', $Games, $Out, '--window', '--max-imports', '1000000000000')
if ($Fullscreen) { $runArgs += '--fullscreen' }
$savedEncoding = [Console]::OutputEncoding
$writer = $null
try {
    foreach ($name in $set.Keys) {
        Set-Item -LiteralPath "Env:$name" -Value $set[$name]
        Write-Host "$name=$($set[$name])"
    }
    # Other MH3U_ variables still set (cache and budget switches) are shown, as they change the run too.
    Get-ChildItem Env: | Where-Object { $_.Name -like 'MH3U_*' -and -not $set.Contains($_.Name) } |
        ForEach-Object { Write-Host "also set: $($_.Name)=$($_.Value)" }
    Write-Host "> $tools $($runArgs -join ' ')"
    Write-Host "log: $Log"
    # The game prints UTF-8; a UTF-8 log without a byte order mark.
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
    $writer = New-Object System.IO.StreamWriter -ArgumentList $Log, $false, (New-Object System.Text.UTF8Encoding $false)
    $writer.AutoFlush = $true
    # Windows PowerShell wraps each line the program writes to stderr in an error record (NativeCommandError/RemoteException)
    # and would stop on the first one under 'Stop'; the lines are turned back into plain text here.
    $ErrorActionPreference = 'Continue'
    & $tools @runArgs 2>&1 | ForEach-Object {
        if ($_ -is [System.Management.Automation.ErrorRecord]) { $line = $_.Exception.Message } else { $line = "$_" }
        $writer.WriteLine($line)
        $line
    }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    Write-Host "exit code $code; log: $Log"
} finally {
    if ($writer) { $writer.Dispose() }
    [Console]::OutputEncoding = $savedEncoding
    foreach ($name in $set.Keys) {
        Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    }
}
exit $code
