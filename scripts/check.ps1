# The checks to run before every commit, stopping at the first failure: the same as scripts/check.sh, for Windows PowerShell.
# Run it from anywhere:  powershell -ExecutionPolicy Bypass -File scripts\check.ps1
$ErrorActionPreference = 'Stop'
# PowerShell 7.3+: a native command's exit code is checked here, not turned into an error
$PSNativeCommandUseErrorActionPreference = $false
Set-Location -LiteralPath (Join-Path $PSScriptRoot '..')

function Step([string]$Command, [string[]]$Arguments) {
    Write-Host "> $Command $($Arguments -join ' ')"
    # native stderr (cargo's progress) is not an error: PowerShell 5 raises it as one under 'Stop' when redirected
    $ErrorActionPreference = 'Continue'
    & $Command @Arguments 2>&1 | ForEach-Object { if ($_ -is [System.Management.Automation.ErrorRecord]) { "$_" } else { $_ } }
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0) {
        $code = $LASTEXITCODE
        [Console]::Error.WriteLine("failed: $Command $($Arguments -join ' ') (exit code $code)")
        exit $code
    }
}

# the tests that need the game's files find them through MH3U_GAME_DIR; without it they skip
Step cargo @('fmt', '--all', '--check')
Step cargo @('clippy', '--all-targets', '--', '-D', 'warnings')
Step cargo @('test', '--all')
# known vulnerabilities in the dependencies (https://rustsec.org); this runs when cargo-audit is installed (cargo install cargo-audit --locked)
if (Get-Command cargo-audit -ErrorAction SilentlyContinue) {
    Step cargo @('audit')
} else {
    Write-Warning 'cargo-audit is not installed, so the dependency audit was skipped here'
}
Write-Host 'all checks passed'
