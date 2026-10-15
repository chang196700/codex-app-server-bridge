[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet(
        'x86_64-unknown-linux-gnu',
        'aarch64-unknown-linux-gnu',
        'x86_64-apple-darwin',
        'aarch64-apple-darwin',
        'x86_64-pc-windows-msvc',
        'aarch64-pc-windows-msvc'
    )]
    [string]$Target
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Invoke-Checked {
    param(
        [Parameter(Mandatory)]
        [string]$Command,
        [string[]]$Arguments
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE."
    }
}

$repository = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Push-Location $repository
try {
    $toolchainConfig = Get-Content -LiteralPath 'rust-toolchain.toml' -Raw
    if ($toolchainConfig -notmatch '(?m)^\s*channel\s*=\s*"([^"]+)"\s*(?:#.*)?$') {
        throw 'Cannot read the Rust channel from rust-toolchain.toml.'
    }
    $toolchain = "$($Matches[1])-$Target"
    Invoke-Checked rustup @('toolchain', 'install', $toolchain, '--profile', 'minimal', '--component', 'rustfmt', '--component', 'clippy', '--no-self-update')
    Invoke-Checked rustup @('override', 'set', $toolchain)
    # This also takes precedence over toolchain files in child directories.
    $env:RUSTUP_TOOLCHAIN = $toolchain

    $rustcVersion = (Invoke-Checked rustc @('--version', '--verbose')) -join "`n"
    Write-Host $rustcVersion
    if ($rustcVersion -notmatch '(?m)^host: (\S+)\s*$' -or $Matches[1] -ne $Target) {
        throw "Expected a native Rust host of $Target."
    }
    $expectedArchitecture = if ($Target.StartsWith('aarch64-')) { 'ARM64' } else { 'X64' }
    $machineArchitecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    if ($machineArchitecture -ne $expectedArchitecture) {
        throw "Expected an $expectedArchitecture runner, found $machineArchitecture."
    }

    Invoke-Checked cargo @('fmt', '--check')
    Invoke-Checked cargo @('clippy', '--all-targets', '--locked', '--', '-D', 'warnings')
    Invoke-Checked cargo @('build', '--release', '--locked')

    $metadata = (Invoke-Checked cargo @('metadata', '--no-deps', '--format-version', '1', '--locked')) | ConvertFrom-Json
    $package = @($metadata.packages | Where-Object { $_.name -eq 'codex-app-server-bridge' })
    if ($package.Count -ne 1) {
        throw 'Expected exactly one codex-app-server-bridge package.'
    }
    $version = $package[0].version
    $executableName = if ($IsWindows) { 'codex-app-server-bridge.exe' } else { 'codex-app-server-bridge' }
    $executable = Join-Path $metadata.target_directory "release/$executableName"
    $reportedVersion = (Invoke-Checked $executable @('--version')) -join "`n"
    if ($reportedVersion.Trim() -ne "codex-app-server-bridge $version") {
        throw "Unexpected executable version: $reportedVersion"
    }
    Write-Host $reportedVersion

    $commit = ((Invoke-Checked git @('rev-parse', 'HEAD')) -join '').Trim()
    $binaryHash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
    $archiveName = "codex-app-server-bridge-v$version-$Target"
    $distribution = Join-Path $repository 'dist'
    $staging = Join-Path $metadata.target_directory "release-package/$archiveName"
    if (Test-Path -LiteralPath $staging) {
        throw "Package staging directory already exists: $staging"
    }
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    New-Item -ItemType Directory -Path $distribution -Force | Out-Null
    Copy-Item -LiteralPath $executable -Destination (Join-Path $staging $executableName)
    Copy-Item -LiteralPath 'README.md', 'README_ZH.md', 'LICENSE-MIT', 'LICENSE-APACHE' -Destination $staging
    @"
Package: codex-app-server-bridge
Version: $version
Target: $Target
Source commit: $commit
Binary SHA256: $binaryHash

$rustcVersion
"@ | Set-Content -LiteralPath (Join-Path $staging 'BUILDINFO.txt') -Encoding utf8NoBOM

    if ($IsWindows) {
        $archive = Join-Path $distribution "$archiveName.zip"
        Compress-Archive -LiteralPath $staging -DestinationPath $archive
    }
    else {
        $archive = Join-Path $distribution "$archiveName.tar.gz"
        Invoke-Checked chmod @('755', (Join-Path $staging $executableName))
        Invoke-Checked tar @('-czf', $archive, '-C', (Split-Path $staging -Parent), $archiveName)
    }
    Write-Host "Created $archive"
}
finally {
    Pop-Location
}
