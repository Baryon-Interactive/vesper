param([int]$Port = 3210, [int]$GatewayPort = 8787, [switch]$SkipSetup)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$out = Join-Path $PSScriptRoot 'out'
if (-not ([IO.Path]::GetFullPath($out)).StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Tour output must stay inside this checkout' }
$screens = Join-Path $out 'screens'
$stateRoot = Join-Path $out 'tour-state'
if (Test-Path $stateRoot) { Remove-Item -Recurse -Force $stateRoot }
$env:VESPER_STATE_DIR = $stateRoot
if (-not $SkipSetup) { & (Join-Path $PSScriptRoot 'start.ps1') }
$indexServer = Start-Process -FilePath 'python' -ArgumentList @((Join-Path $PSScriptRoot 'server.py'),'--root',$out,'--port',$Port) -PassThru -WindowStyle Hidden
$gatewayServer = Start-Process -FilePath 'python' -ArgumentList @((Join-Path $PSScriptRoot 'server.py'),'--root',$out,'--port',$GatewayPort) -PassThru -WindowStyle Hidden

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
function Get-Root {
  $process = Get-Process | Where-Object { $_.MainWindowTitle -eq 'Vesper' } | Select-Object -First 1
  if (-not $process) { throw 'Vesper window not found' }
  [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
}
function Get-Elements { (Get-Root).FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) }
function Text-Dump { (Get-Elements | ForEach-Object { $_.Current.Name } | Where-Object { $_ } | Select-Object -Unique) -join ' | ' }
function Wait-Text([string]$Expected, [int]$Timeout = 60) {
  $deadline = [DateTime]::UtcNow.AddSeconds($Timeout)
  do { $actual = Text-Dump; if ($actual -like "*$Expected*") { return $actual }; Start-Sleep -Milliseconds 300 } while ([DateTime]::UtcNow -lt $deadline)
  throw "Expected '$Expected' was not visible. Actual UI text: $actual"
}
function Click-Button([string]$Name, [int]$Timeout = 60, [int]$Occurrence = 0) {
  $deadline = [DateTime]::UtcNow.AddSeconds($Timeout)
  do {
    $found = @(Get-Elements | Where-Object { $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button -and $_.Current.Name -like $Name -and $_.Current.IsEnabled })
    if ($found.Count -gt $Occurrence) { $found[$Occurrence].GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); return }
    Start-Sleep -Milliseconds 300
  } while ([DateTime]::UtcNow -lt $deadline)
  throw "Enabled button '$Name' was not found. Actual UI text: $(Text-Dump)"
}
function Set-Edit([string]$Name, [string]$Value, [int]$Timeout = 30) {
  $deadline = [DateTime]::UtcNow.AddSeconds($Timeout)
  do {
    $found = @(Get-Elements | Where-Object { $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Edit -and ($_.Current.Name -eq $Name -or $_.Current.HelpText -eq $Name) })
    if ($found.Count) { $found[0].GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value); return }
    Start-Sleep -Milliseconds 300
  } while ([DateTime]::UtcNow -lt $deadline)
  throw "Edit '$Name' was not found. Actual UI text: $(Text-Dump)"
}
function Capture([string]$Name, [string[]]$Expected) {
  foreach ($text in $Expected) { Wait-Text $text | Out-Null; Write-Host "ASSERT $Name contains '$text'" }
  & (Join-Path $PSScriptRoot 'screenshot-window.ps1') -Out (Join-Path $screens "$Name.png") -Title 'Vesper'
  Write-Host "SCREEN $Name.png :: $(Text-Dump)"
}

try {
  if (-not $SkipSetup) { & (Join-Path $PSScriptRoot 'build-exe.ps1') }
  if (Test-Path $screens) { Remove-Item -Recurse -Force $screens }
  New-Item -ItemType Directory -Force -Path $screens | Out-Null
  $process = Start-Process -FilePath (Join-Path $out 'bin/vesper-launcher.exe') -PassThru
  try {
    Start-Sleep -Seconds 5
    Capture '01-store' @('Lanterns at Dawn', 'Mossbound', 'Cover for Lanterns at Dawn', 'Cover for Mossbound')
    Click-Button 'Lanterns at Dawn'
    Capture '02-detail' @('TITLE DETAILS', 'Lanterns at Dawn')
    Click-Button 'Close'
    Click-Button 'Install Lanterns at Dawn*'
    Capture '03-installing' @('Downloads', 'demo.free')
    Click-Button 'Library'
    Capture '04-library-v1' @('Your library', 'Lanterns at Dawn', 'Version 1', 'stable')
    & (Join-Path $PSScriptRoot 'publish-v2.ps1')
    # A fresh v2 library must download unchanged packs from release 1.
    $freshLibrary = Join-Path $stateRoot 'fresh-library'
    Click-Button 'Settings'
    Set-Edit 'Library directory' $freshLibrary
    Click-Button 'Save settings*'
    Click-Button 'Store'
    Click-Button 'Install Lanterns at Dawn*'
    Click-Button 'Library'
    Capture '05-fresh-v2' @('Lanterns at Dawn', 'Version 2', 'stable')
    $expectedRoot = Join-Path $PSScriptRoot 'work/builds/free'
    foreach ($file in (Get-ChildItem -LiteralPath $expectedRoot -File -Recurse)) {
      $relative = $file.FullName.Substring($expectedRoot.Length).TrimStart('\','/')
      $installedFile = Join-Path $freshLibrary "demo.free/$relative"
      if (-not (Test-Path -LiteralPath $installedFile) -or (Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath $installedFile).Hash) { throw "Fresh v2 file mismatch: $relative" }
    }
    Click-Button 'Verify'
    Wait-Text 'demo.free verified' | Out-Null
    Write-Host 'PASS fresh v2 install across release directories and file verification'
    Click-Button 'Settings'
    Set-Edit 'Library directory' (Join-Path $stateRoot 'library')
    Click-Button 'Save settings*'
    Click-Button 'Store'
    Click-Button 'Library'
    Capture '05-update-available' @('Update available: v2', 'Version 1')
    Click-Button 'Update'
    Click-Button 'Library'
    Capture '06-library-v2' @('Lanterns at Dawn', 'Version 2', 'stable')
    Click-Button 'Rollback'
    Capture '07-rollback' @('Lanterns at Dawn', 'Version 1', 'stable')
    $readme = Join-Path $stateRoot 'library/demo.free/README.txt'
    if (-not (Test-Path $readme)) { throw "Missing installed file $readme" }
    [IO.File]::WriteAllText($readme, 'corrupted by tour', [Text.UTF8Encoding]::new($false))
    Click-Button 'Verify'
    Wait-Text 'Repair' | Out-Null
    Click-Button 'Repair'
    Click-Button 'Library'
    Capture '08-repair' @('Lanterns at Dawn', 'Version 1', 'Repaired')
    Click-Button '*Guest session*'
    Capture '09-login' @('Sign in to Vesper', 'Guest session')
    Set-Edit 'you@example.com' 'buyer@example.com'
    Click-Button 'Send verification code'
    Set-Edit '6-digit code' '123456'
    Click-Button 'Verify and sign in*'
    Wait-Text 'buyer@example.com' | Out-Null
    Set-Edit 'Redeem code' 'DEMO-PAID-2026'
    Click-Button 'Redeem code'
    Click-Button 'Store'
    Wait-Text 'Mossbound' | Out-Null
    Click-Button 'Mossbound'
    Click-Button 'Close'
    Click-Button 'Install Mossbound*'
    Click-Button 'Library'
    Wait-Text 'Mossbound' | Out-Null
    Capture '10-paid-installed' @('Mossbound', 'PAID')
    Click-Button 'Settings'
    Capture '11-settings' @('Launcher settings', 'Library location')
    Click-Button 'Library'
    Wait-Text 'Mossbound' | Out-Null
    Click-Button 'Uninstall' 60 1
    Wait-Text 'Confirm uninstall' | Out-Null
    Click-Button 'Keep user files'
    Capture '12-uninstall-paid' @('Your library', 'Lanterns at Dawn')
  } finally { if ($process -and !$process.HasExited) { Stop-Process -Id $process.Id -Force } }
} finally {
  Remove-Item Env:VESPER_STATE_DIR -ErrorAction SilentlyContinue
  if ($indexServer -and !$indexServer.HasExited) { Stop-Process -Id $indexServer.Id -Force }
  if ($gatewayServer -and !$gatewayServer.HasExited) { Stop-Process -Id $gatewayServer.Id -Force }
}
Write-Host 'PASS fresh v2 install uses old and new release packs'
Write-Host 'PASS all launcher tour steps'
Write-Host "Tour screenshots written to $screens"
