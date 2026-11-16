[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,
    [Parameter(Mandatory = $true)]
    [string]$OutDir,
    [switch]$AllowDirty
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent $PSScriptRoot
$linuxImage = 'rust:1.99.1-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383'
$systemTar = Join-Path $env:SystemRoot 'System32/tar.exe'
if (-not (Test-Path -LiteralPath $systemTar -PathType Leaf)) { throw "Windows system tar is required: $systemTar" }
$windowsToolchain = '1.99.1-x86_64-pc-windows-msvc'
$vsDevCmd = (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/2019/BuildTools/Common7/Tools/VsDevCmd.bat')

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Command failed ($LASTEXITCODE): $Program $($Arguments -join ' ')" }
}

$status = & git -C $repo status --porcelain --untracked-files=all
if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect the source tree.' }
if ($status -and -not $AllowDirty) {
    throw 'The working tree is dirty. Commit changes first, or use -AllowDirty to build HEAD only.'
}
if ($status) { Write-Warning 'Ignoring all uncommitted changes; the release still uses git archive HEAD.' }
$commit = (& git -C $repo rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve HEAD.' }
$epoch = (& git -C $repo show -s --format=%ct HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot read the source commit timestamp.' }
$output = [System.IO.Path]::GetFullPath($OutDir)
if ($output.Contains('"') -or $output.Contains(',')) { throw 'Output paths cannot contain quotes or commas.' }
if (-not (Test-Path -LiteralPath $vsDevCmd)) { throw "Missing MSVC developer command: $vsDevCmd" }

# All scratch writes stay inside the checkout's ignored target directory.
$scratchRoot = Join-Path $repo 'target/vesper-release'
$scratch = Join-Path $scratchRoot ([guid]::NewGuid().ToString('N'))
$source = Join-Path $scratch 'source'
$environmentNames = @('VESPER_GIT_SHA', 'SOURCE_DATE_EPOCH', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS',
    'CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'CARGO_PROFILE_RELEASE_STRIP', 'RUSTUP_TOOLCHAIN')
$savedEnvironment = @{}
foreach ($name in $environmentNames) { $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }

try {
    New-Item -ItemType Directory -Path $source -Force | Out-Null
    $archive = Join-Path $scratch 'source.tar'
    Invoke-Checked 'git' @('-C', $repo, 'archive', '--format=tar', "--output=$archive", $commit)
    Invoke-Checked $systemTar @('-xf', $archive, '-C', $source)
    $manifest = Get-Content -LiteralPath (Join-Path $source 'Cargo.toml') -Raw -Encoding UTF8
    $packageVersion = [regex]::Match($manifest, '(?m)^version = "([^"]+)"\r?$').Groups[1].Value
    if ($Version -ne $packageVersion) { throw "Requested version $Version does not match archived workspace version $packageVersion." }
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    $env:VESPER_GIT_SHA = $commit.Substring(0, 12)
    $env:SOURCE_DATE_EPOCH = $epoch
    $env:CARGO_INCREMENTAL = '0'
    $env:CARGO_PROFILE_RELEASE_STRIP = 'symbols'
    $env:CARGO_ENCODED_RUSTFLAGS = $null
    $env:RUSTUP_TOOLCHAIN = $windowsToolchain
    $env:CARGO_TARGET_DIR = Join-Path $scratch 'windows-target'
    # Remap the random archive path and remove PE link timestamps for repeat builds.
    $env:RUSTFLAGS = "--remap-path-prefix=$source=/src -C link-arg=/Brepro"

    Write-Host "Building $Version from $commit with $linuxImage"
    # These build packages are already installed in the pinned image; do not update them.
    # Keep linker intermediates off the Windows bind mount; export only the final binary.
    $linuxBuild = 'set -eu; apk info -e musl-dev binutils > /dev/null; cargo build --release --locked -p vesper-cli --bin vesper --target x86_64-unknown-linux-musl; strip /tmp/vesper-target/x86_64-unknown-linux-musl/release/vesper; cp /tmp/vesper-target/x86_64-unknown-linux-musl/release/vesper /src/vesper-cli-linux-x64'
    Invoke-Checked 'docker' @('run', '--rm', '--platform', 'linux/amd64',
        '--mount', "type=bind,source=$source,target=/src", '--workdir', '/src',
        '-e', "VESPER_GIT_SHA=$env:VESPER_GIT_SHA", '-e', "SOURCE_DATE_EPOCH=$epoch",
        '-e', 'RUSTUP_TOOLCHAIN=1.99.1', '-e', 'CARGO_INCREMENTAL=0',
        '-e', 'CARGO_TARGET_DIR=/tmp/vesper-target',
        '-e', 'CARGO_PROFILE_RELEASE_STRIP=symbols', $linuxImage, 'sh', '-c', $linuxBuild)

    $windowsBuild = Join-Path $scratch 'build-windows.cmd'
    $cmdText = "@echo off`r`ncall `"$vsDevCmd`" -arch=x64 >nul`r`nif errorlevel 1 exit /b %errorlevel%`r`ncd /d `"$source`"`r`ncargo +$windowsToolchain build --release --locked -p vesper-cli --bin vesper --target x86_64-pc-windows-msvc`r`nexit /b %errorlevel%`r`n"
    [System.IO.File]::WriteAllText($windowsBuild, $cmdText, [System.Text.Encoding]::ASCII)
    Invoke-Checked 'cmd.exe' @('/d', '/c', $windowsBuild)

    $linuxBinary = Join-Path $source 'vesper-cli-linux-x64'
    $windowsBinary = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/release/vesper.exe'
    $expected = "vesper $Version ($env:VESPER_GIT_SHA)"
    $actual = (& $windowsBinary --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $actual -ne $expected) { throw "Unexpected Windows version: $actual" }
    $actual = (& docker run --rm --platform linux/amd64 --mount "type=bind,source=$source,target=/src,readonly" $linuxImage /src/vesper-cli-linux-x64 --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $actual -ne $expected) { throw "Unexpected Linux version: $actual" }
    Copy-Item -LiteralPath $linuxBinary -Destination (Join-Path $output 'vesper-cli-linux-x64')
    Copy-Item -LiteralPath $windowsBinary -Destination (Join-Path $output 'vesper-cli-windows-x64.exe')
    # License texts come from the archived commit, so they match the source of the binaries.
    foreach ($name in @('LICENSE', 'NOTICE')) { Copy-Item -LiteralPath (Join-Path $source $name) -Destination (Join-Path $output $name) }
    $sums = foreach ($name in @('vesper-cli-linux-x64', 'vesper-cli-windows-x64.exe', 'LICENSE', 'NOTICE')) {
        $hash = (Get-FileHash -LiteralPath (Join-Path $output $name) -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $name"
    }
    [System.IO.File]::WriteAllText((Join-Path $output 'SHA256SUMS'), (($sums -join "`n") + "`n"), [System.Text.UTF8Encoding]::new($false))
    Write-Host "Source: $commit"
    Write-Host "Windows toolchain: $windowsToolchain"
    $sums | ForEach-Object { Write-Host $_ }
}
finally {
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process') }
    # Verify the absolute cleanup target before recursively removing scratch files.
    $resolvedScratch = [System.IO.Path]::GetFullPath($scratch)
    $resolvedRoot = [System.IO.Path]::GetFullPath($scratchRoot).TrimEnd('\') + '\'
    if (-not $resolvedScratch.StartsWith($resolvedRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Unsafe scratch cleanup path: $resolvedScratch"
    }
    if (Test-Path -LiteralPath $resolvedScratch) { Remove-Item -LiteralPath $resolvedScratch -Recurse -Force }
}
