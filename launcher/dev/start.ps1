param(
  [string]$Work = (Join-Path $PSScriptRoot 'work'),
  [int]$Port = 3210
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$platform = Join-Path $repo 'index-template'
$Work = [IO.Path]::GetFullPath($Work)
if (-not $Work.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Demo work directory must stay inside this checkout' }

$vsdev = @(
  (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat'),
  $env:VESPER_VCVARS64
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $vsdev) { throw 'Visual Studio developer command script was not found' }
$env:PATH = ((Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer') + ';') + $env:PATH
$out = Join-Path $PSScriptRoot 'out'
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
$index = Join-Path $Work 'index'
$keys = Join-Path $Work 'keys'
$builds = Join-Path $Work 'builds'
New-Item -ItemType Directory -Force -Path $index,$keys,$builds,$out | Out-Null
 $demoManifest = Join-Path $PSScriptRoot 'demo-game/Cargo.toml'
 $demoLine = 'call "' + $vsdev + '" >nul && cargo build --release --manifest-path "' + $demoManifest + '"'
 cmd /c ($demoLine + ' 2>&1') | Out-Host
 if ($LASTEXITCODE -ne 0) { throw 'demo-game build failed' }
 $demoExe = Join-Path $PSScriptRoot 'demo-game/target/release/demo-game.exe'
function New-KeyHex {
  return (([Guid]::NewGuid().ToString('N')) + ([Guid]::NewGuid().ToString('N')))
}
function Invoke-Vesper([string[]]$VesperArgs) {
  $quoted = ($VesperArgs | ForEach-Object { '"' + $_.Replace('"','\"') + '"' }) -join ' '
  $line = 'call "' + $vsdev + '" >nul && cargo run --manifest-path "' + $repo + '\Cargo.toml" -p vesper-cli -- ' + $quoted
  $output = cmd /c ($line + ' 2>&1')
  if ($LASTEXITCODE -ne 0) { throw "vesper failed: $($VesperArgs -join ' ') $output" }
  return $output
}
function Write-Utf8([string]$Path, [string]$Text) {
  [System.IO.File]::WriteAllText($Path, $Text, [System.Text.UTF8Encoding]::new($false))
}
# Reuse the tracked app icon as a deterministic cover fixture; no generated art.
$coverPath = Join-Path $repo 'launcher/src-tauri/icons/icon.ico'
$helperManifest = Join-Path $PSScriptRoot 'sign-paid/Cargo.toml'
$hashLine = 'call "' + $vsdev + '" >nul && cargo run --manifest-path "' + $helperManifest + '" -- --media-hash "' + $coverPath + '"'
$hashOutput = cmd /c ($hashLine + ' 2>&1')
if ($LASTEXITCODE -ne 0) { throw 'Cover hash helper failed' }
$coverHash = $hashOutput | Where-Object { $_ -match '^[0-9a-f]{64}$' } | Select-Object -Last 1
if (-not $coverHash) { throw 'Missing cover BLAKE3 hash' }
$env:VESPER_ONLINE_KEY = New-KeyHex
Write-Utf8 (Join-Path $Work 'online.key') $env:VESPER_ONLINE_KEY
$onlinePublic = Invoke-Vesper @('index','online-pubkey') | Where-Object { $_ -match '^[0-9a-f]{64}$' } | Select-Object -Last 1
if (-not $onlinePublic) { throw 'Missing online public key' }
Invoke-Vesper @('index','init','--index',$index,'--root-keys','1','--threshold','1','--out-keys',$keys,'--online-pubkey',$onlinePublic) | Out-Host
$apps = @(
  @{ Id='demo.free'; Title='Lanterns at Dawn'; Summary='A free verified demo build.'; Paid=$false; Version='1'; Release='free-1'; Folder='free'; Price='Free'; Tag='demo' },
  @{ Id='demo.paid'; Title='Mossbound'; Summary='A paid encrypted demo build.'; Paid=$true; Version='1'; Release='paid-1'; Folder='paid'; Price='$4.99'; Tag='adventure' }
)
$rootKey = Join-Path $keys 'root-0.key'
foreach ($app in $apps) {
  $folder = Join-Path $builds $app.Folder
  New-Item -ItemType Directory -Force -Path $folder | Out-Null
  Copy-Item $demoExe (Join-Path $folder 'game.exe') -Force
  Write-Utf8 (Join-Path $folder 'VERSION.txt') '1'
  $large = New-Object byte[] (2MB)
  for ($i = 0; $i -lt $large.Length; $i++) { $large[$i] = [byte](($i * 31 + 7) % 251) }
  [IO.File]::WriteAllBytes((Join-Path $folder 'content.bin'), $large)
  Write-Utf8 (Join-Path $folder 'README.txt') ("Installed by Vesper: $($app.Title)" + [Environment]::NewLine)
  $pack = Join-Path $Work "$($app.Folder)-pack"
  $env:VESPER_DEV_KEY = New-KeyHex
  $env:VESPER_APP_ID = $app.Id
  Write-Utf8 (Join-Path $Work "$($app.Folder).devkey") $env:VESPER_DEV_KEY
  $config = Join-Path $Work "$($app.Folder).toml"
  $initOutput = Invoke-Vesper @('init','--output',$config,'--app-id',$app.Id)
  $public = ($initOutput | Select-String 'developer_public_key=' | ForEach-Object { ($_ -split '=',2)[1].Trim() } | Select-Object -First 1)
  if (-not $public) { throw "missing developer public key for $($app.Id)" }
  Invoke-Vesper @('index','route-add','--index',$index,'--app',$app.Id,'--pubkey',$public) | Out-Host
  Invoke-Vesper @('index','route-sign','--index',$index,'--root-key-file',$rootKey) | Out-Host
  $storeDir = Join-Path $index "apps/$($app.Id)"
  New-Item -ItemType Directory -Force -Path $storeDir | Out-Null
  $store = @{
    title = @{ en = $app.Title; 'zh-CN' = $app.Title }
    summary = @{ en = $app.Summary; 'zh-CN' = $app.Summary }
    short_description = $app.Summary
    tags = @($app.Tag, 'windows')
    screenshots = @()
    cover = @{ url = "http://127.0.0.1:$Port/cover.ico"; blake3 = $coverHash }
    requirements = @{ os = 'Windows 10 64-bit'; cpu = '2 cores'; ram = '2 GB'; gpu = 'DX11' }
    paid = $app.Paid
    price_display = $app.Price
    developer = 'Vesper Demo'
    homepage = 'http://127.0.0.1:3210'
    platforms = @('windows-x64')
  }
  Write-Utf8 (Join-Path $storeDir 'store.json') (($store | ConvertTo-Json -Depth 8) + [Environment]::NewLine)
  Write-Utf8 (Join-Path $storeDir 'description.md') ("# $($app.Title)" + [Environment]::NewLine + [Environment]::NewLine + $app.Summary + [Environment]::NewLine)
  if ($app.Paid) {
    $publicPack = Join-Path $Work "$($app.Folder)-public-pack"
    Invoke-Vesper @('pack','--input',$folder,'--output',$publicPack,'--app-id',$app.Id,'--platform','windows-x64','--version',$app.Version,'--release-tag',$app.Release) | Out-Host
    Invoke-Vesper @('publish',$publicPack,'--channel','stable','--index',$index) | Out-Host
    Invoke-Vesper @('pack','--input',$folder,'--output',$pack,'--app-id',$app.Id,'--platform','windows-x64','--version',$app.Version,'--release-tag',$app.Release,'--depot-key-hex',('11' * 32),'--key-id','1') | Out-Host
    $targetPath = Join-Path $index "apps/$($app.Id)/targets.json"
    $helperManifest = Join-Path $PSScriptRoot 'sign-paid/Cargo.toml'
    $helperArgs = @('run','--manifest-path',$helperManifest,'--',$targetPath,(Join-Path $pack 'manifest.cbor.zst'),$app.Release,$app.Id)
    $helperQuoted = ($helperArgs | ForEach-Object { '"' + $_.Replace('"','\"') + '"' }) -join ' '
    $helperLine = 'call "' + $vsdev + '" >nul && cargo ' + $helperQuoted
    cmd /c ($helperLine + ' 2>&1') | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "paid target signing failed" }
  } else {
    $packArgs = @('pack','--input',$folder,'--output',$pack,'--app-id',$app.Id,'--platform','windows-x64','--version',$app.Version,'--release-tag',$app.Release)
    Invoke-Vesper $packArgs | Out-Host
    Invoke-Vesper @('publish',$pack,'--channel','stable','--index',$index) | Out-Host
  }

}
foreach ($app in $apps) { Invoke-Vesper @('index','snapshot','--index',$index,'--app',$app.Id,'--initialize') | Out-Host }
Invoke-Vesper @('index','timestamp','--index',$index,'--all') | Out-Host
node (Join-Path $platform 'scripts/build-api.mjs') --index $index --out $out
Copy-Item -LiteralPath $coverPath -Destination (Join-Path $out 'cover.ico')
foreach ($app in $apps) {
  $pack = Join-Path $Work "$($app.Folder)-pack"
  $appOut = Join-Path $out "apps/$($app.Id)"
  Copy-Item (Join-Path $pack 'manifest.cbor.zst') (Join-Path $appOut 'manifest.cbor.zst') -Force
  $releaseOut = Join-Path $out $app.Release
  New-Item -ItemType Directory -Force -Path $releaseOut | Out-Null
  Copy-Item (Join-Path $pack 'packs/*') $releaseOut -Force
  Copy-Item (Join-Path $pack 'manifest.cbor.zst') (Join-Path $releaseOut 'manifest.cbor.zst') -Force
}
Write-Host "Static API output: $out"
Write-Host "Start local services in another terminal:"
Write-Host "  python $PSScriptRoot/server.py --root $out --port $Port"
Write-Host "Paid demo redeem code: DEMO-PAID-2026"
