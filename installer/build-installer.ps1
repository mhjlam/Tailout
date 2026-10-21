#Requires -Version 7.0
[CmdletBinding()]
param(
    [switch]$Offline,
    [switch]$SkipBuild,
    [string]$IsccPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'The Tailout Windows installer must be built on Windows.' }
$projectRoot = Split-Path -Parent $PSScriptRoot
$manifest = Get-Content -LiteralPath (Join-Path $projectRoot 'Cargo.toml') -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"\s*$') {
    throw 'Cargo.toml must contain a three-part application version.'
}
$appVersion = $Matches[1]

if (-not $IsccPath) {
    $compiler = Get-Command iscc.exe -ErrorAction SilentlyContinue
    if ($compiler) { $IsccPath = $compiler.Source }
    else {
        foreach ($candidate in @(
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
        )) {
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $IsccPath = $candidate
                break
            }
        }
    }
}
if (-not $IsccPath -or -not (Test-Path -LiteralPath $IsccPath -PathType Leaf)) {
    throw 'Install Inno Setup 6.3 or newer, add ISCC.exe to PATH, or pass -IsccPath.'
}

Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipBuild) {
        $cargoArguments = @('build', '--release', '--locked')
        if ($Offline) { $cargoArguments += '--offline' }
        & cargo @cargoArguments
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed. Close any running release copy before rebuilding.' }
    }
    $appSource = Join-Path $projectRoot 'target\release\tailout.exe'
    if (-not (Test-Path -LiteralPath $appSource -PathType Leaf)) {
        throw "Release executable not found: $appSource"
    }
    # Prevent accidentally labeling an x86/ARM/cross-platform build as x64.
    $reader = [System.IO.BinaryReader]::new([System.IO.File]::OpenRead($appSource))
    try {
        if ($reader.ReadUInt16() -ne 0x5a4d) { throw 'Release executable is not a Windows PE file.' }
        $reader.BaseStream.Position = 0x3c
        $peOffset = $reader.ReadInt32()
        $reader.BaseStream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw 'This installer requires an x64 Windows release executable.'
        }
    }
    finally { $reader.Dispose() }

    $outputDirectory = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    & $IsccPath '/Q' "/DAppVersion=$appVersion" "/DAppSource=$appSource" "/DInstallerOutput=$outputDirectory" (Join-Path $PSScriptRoot 'Tailout.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed.' }
    $installer = Join-Path $outputDirectory "Tailout-$appVersion-windows-x64-setup.exe"
    $hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $([System.IO.Path]::GetFileName($installer))" |
        Set-Content -LiteralPath "$installer.sha256" -Encoding ascii
    Write-Host "Installer: $installer"
    Write-Host "SHA256:    $hash"
}
finally { Pop-Location }
