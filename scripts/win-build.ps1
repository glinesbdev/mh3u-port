# Builds the tools and the game library on Windows (Windows PowerShell 5.1 or PowerShell 7).
#   powershell -ExecutionPolicy Bypass -File scripts\win-build.ps1                  # tools, then game.dll for this processor
#   powershell -ExecutionPolicy Bypass -File scripts\win-build.ps1 -Profile -Lto    # with PDBs for a profiler, and thin LTO
# It translates the game first when -Out has no translated C yet (or with -Translate). See docs/windows.md.
[CmdletBinding()]
param(
    # Write PDBs (this hides $PROFILE inside the script only): for mh3u-tools.exe (RUSTFLAGS) and game.dll (MH3U_CFLAGS), for Visual Studio or another profiler.
    [switch]$Profile,
    # Optimise the game's C across files when linking (build-game --lto; needs clang and lld; the link takes longer).
    [switch]$Lto,
    # A debugging build that reports stores to a watched range (build-game --watch; slower). Use a separate -Out for it. See docs/hooks.md.
    [switch]$Watch,
    # Translate the game into -Out again first (needed after the translator changes; done anyway when -Out has no game.h).
    [switch]$Translate,
    # Where the translated C and game.dll go (outside any git working tree).
    [string]$Out = (Join-Path $env:LOCALAPPDATA 'mh3u\c'),
    # The folder with the [Game] and [Update] dumps.
    [string]$Games = (Join-Path $env:USERPROFILE 'Downloads\wiiu')
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
# full paths, from where the script was started (they need not exist yet)
$Games = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Games)
$Out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)

function Step([string]$Command, [string[]]$Arguments) {
    Write-Host "> $Command $($Arguments -join ' ')"
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "failed: $Command $($Arguments -join ' ') (exit code $LASTEXITCODE)"
    }
}

# The variables set here are put back afterwards, as the script may run in the caller's session.
$saved = @{ RUSTFLAGS = $env:RUSTFLAGS; MH3U_CFLAGS = $env:MH3U_CFLAGS }
Push-Location -LiteralPath $repo
try {
    if ($Profile) {
        # A different RUSTFLAGS rebuilds every crate once; a later build without -Profile rebuilds them again.
        $env:RUSTFLAGS = '-C debuginfo=1 -C link-arg=-Wl,--pdb='
        $env:MH3U_CFLAGS = '-g -gcodeview -Wl,--pdb='
        Write-Host "profile build: RUSTFLAGS='$env:RUSTFLAGS', MH3U_CFLAGS='$env:MH3U_CFLAGS'"
    }
    Step cargo @('build', '--release', '-p', 'mh3u-tools')
    $tools = Join-Path $repo 'target\release\mh3u-tools.exe'
    if (-not (Test-Path -LiteralPath $tools)) {
        throw "no $tools after the build"
    }
    if ($Translate -or -not (Test-Path -LiteralPath (Join-Path $Out 'game.h'))) {
        if (-not (Test-Path -LiteralPath $Games)) {
            throw "no games folder at $Games (pass -Games)"
        }
        Step $tools @('translate', $Games, $Out)
    }
    $buildArgs = @('build-game', $Out, '--native')
    if ($Lto) {
        $buildArgs += '--lto'
    }
    if ($Watch) {
        $buildArgs += '--watch'
    }
    Step $tools $buildArgs
    Write-Host "built $tools and $(Join-Path $Out 'game.dll')"
} finally {
    Pop-Location
    foreach ($name in $saved.Keys) {
        if ($null -eq $saved[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            Set-Item -LiteralPath "Env:$name" -Value $saved[$name]
        }
    }
}
