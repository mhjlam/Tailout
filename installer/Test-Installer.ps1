#Requires -Version 7.0
[CmdletBinding()]
param([string]$InstallerPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'This installer smoke test requires Windows.' }
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $InstallerPath) {
    $InstallerPath = (Get-ChildItem -LiteralPath (Join-Path $projectRoot 'dist') -Filter '*-setup.exe' |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
}
$InstallerPath = (Resolve-Path -LiteralPath $InstallerPath).Path
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{CE0BC667-7DD2-44F9-A137-50B844307B03}_is1'
if (Test-Path -LiteralPath $uninstallKey) {
    throw 'Tailout is already installed. Refusing to alter an existing installation.'
}
$testRoot = Join-Path $projectRoot ".cache\installer smoke $([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $testRoot -Force | Out-Null
$appFolder = Join-Path $testRoot 'Tailout'
$uninstaller = Join-Path $appFolder 'unins000.exe'
$marker = Join-Path $appFolder 'user-preserved.txt'
$startMenu = [Environment]::GetFolderPath('Programs')
$testGroup = 'Tailout'
$shortcut = Join-Path $startMenu "$testGroup\Tailout.lnk"
if (Test-Path -LiteralPath $shortcut) { throw 'Refusing to alter an existing Tailout Start menu shortcut.' }
$preferenceFile = Join-Path $env:APPDATA 'Tailout\session.json'
$preferenceBefore = if (Test-Path -LiteralPath $preferenceFile) {
    (Get-FileHash -LiteralPath $preferenceFile).Hash
} else { '' }

function Invoke-TestSetup([string]$LogName) {
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART',
        '/NOCLOSEAPPLICATIONS', '/NORESTARTAPPLICATIONS', '/TASKS=',
        "/DIR=`"$appFolder`"", "/LOG=`"$(Join-Path $testRoot $LogName)`"")
    $setup = Start-Process -FilePath $InstallerPath -ArgumentList $arguments -WindowStyle Hidden -PassThru
    if (-not $setup.WaitForExit(60000)) { throw "Setup did not finish. Test process ID: $($setup.Id)" }
    if ($setup.ExitCode -ne 0) { throw "Setup failed with exit code $($setup.ExitCode)." }
}
function Invoke-TestUninstall {
    if (-not (Test-Path -LiteralPath $uninstallKey)) { return }
    $registered = (Get-ItemProperty -LiteralPath $uninstallKey).InstallLocation.TrimEnd('\')
    if ($registered -ne $appFolder) { throw 'Refusing to uninstall an installation outside the smoke-test folder.' }
    $remove = Start-Process -FilePath $uninstaller -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES',
        '/NORESTART', "/LOG=`"$(Join-Path $testRoot 'uninstall.log')`"") -WindowStyle Hidden -PassThru
    if (-not $remove.WaitForExit(60000)) { throw "Uninstall did not finish. Test process ID: $($remove.Id)" }
    if ($remove.ExitCode -ne 0) { throw "Uninstall failed with exit code $($remove.ExitCode)." }
}
try {
    Invoke-TestSetup 'install.log'
    $installedExe = Join-Path $appFolder 'tailout.exe'
    if ((Get-FileHash -LiteralPath $installedExe).Hash -ne
        (Get-FileHash -LiteralPath (Join-Path $projectRoot 'target\release\tailout.exe')).Hash) {
        throw 'Installed executable does not match the release build.'
    }
    if (-not (Test-Path -LiteralPath $shortcut)) { throw 'Start menu shortcut was not created.' }
    $shell = New-Object -ComObject WScript.Shell
    if ($shell.CreateShortcut($shortcut).TargetPath -ne $installedExe) { throw 'Shortcut points to the wrong executable.' }
    Set-Content -LiteralPath $marker -Value 'Keep user files on upgrade and uninstall.'
    Invoke-TestSetup 'upgrade.log'
    if (-not (Test-Path -LiteralPath $marker)) { throw 'Repeat installation removed a user file.' }
    Invoke-TestUninstall
    if (Test-Path -LiteralPath $installedExe) { throw 'Uninstall left the application executable behind.' }
    if (Test-Path -LiteralPath $uninstallKey) { throw 'Uninstall left its registration behind.' }
    if (Test-Path -LiteralPath $shortcut) { throw 'Uninstall left its Start menu shortcut behind.' }
    if (-not (Test-Path -LiteralPath $marker)) { throw 'Uninstall removed a user file.' }
    $preferenceAfter = if (Test-Path -LiteralPath $preferenceFile) {
        (Get-FileHash -LiteralPath $preferenceFile).Hash
    } else { '' }
    if ($preferenceBefore -ne $preferenceAfter) { throw 'Installer changed saved Tailout preferences.' }
    Write-Host 'PASS: install, executable hash, Start menu shortcut, repeat install, uninstall, user-file and preference preservation.'
    Write-Host "Logs: $testRoot"
}
finally {
    Invoke-TestUninstall
    # Remove only the marker created by this test. Keep logs for review.
    if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker }
}
