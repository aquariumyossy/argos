# Argos v1.0 release packaging script
$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

# Add Rust / cargo to PATH
$CargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path $CargoBin) {
    $env:PATH = "$CargoBin;$env:PATH"
}

# Load MSVC environment when available
$VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path $VsWhere) {
    $VsPath = & $VsWhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($VsPath) {
        $VcVars = Join-Path $VsPath "VC\Auxiliary\Build\vcvars64.bat"
        if (Test-Path $VcVars) {
            Write-Host "Loading MSVC environment..."
            cmd /c "`"$VcVars`" && set" | ForEach-Object {
                if ($_ -match '^([^=]+)=(.*)$') {
                    [System.Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
                }
            }
        }
    }
}

$Version = (Get-Content (Join-Path $Root "package.json") -Raw | ConvertFrom-Json).version
$OutDir = Join-Path $Root "release\Argos-v$Version"
$BundleDir = Join-Path $Root "src-tauri\target\release\bundle\nsis"

Write-Host "=== Argos v$Version package ==="
Write-Host "Building (first run may take a long time)..."

npm run tauri -- build
if ($LASTEXITCODE -ne 0) {
    throw "tauri build failed (exit $LASTEXITCODE)"
}

if (-not (Test-Path $BundleDir)) {
    throw "Installer directory not found: $BundleDir"
}

$Setup = Get-ChildItem $BundleDir -Filter "*-setup.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $Setup) {
    throw "NSIS setup exe not found"
}

if (Test-Path $OutDir) {
    Remove-Item $OutDir -Recurse -Force
}
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

$DestName = "Argos-Setup-v$Version.exe"
Copy-Item $Setup.FullName (Join-Path $OutDir $DestName)

# Also include portable exe
$Exe = Join-Path $Root "src-tauri\target\release\argos.exe"
if (Test-Path $Exe) {
    $PortableDir = Join-Path $OutDir "portable"
    New-Item -ItemType Directory -Path $PortableDir -Force | Out-Null
    Copy-Item $Exe (Join-Path $PortableDir "Argos.exe")
}

# Japanese install guide (template with version placeholder)
$GuideSrc = Join-Path $PSScriptRoot "install-guide.ja.txt"
$GuideText = (Get-Content $GuideSrc -Raw -Encoding UTF8).Replace("{VERSION}", $Version)
$Utf8Bom = New-Object System.Text.UTF8Encoding $true
[System.IO.File]::WriteAllText((Join-Path $OutDir "INSTALL.txt"), $GuideText, $Utf8Bom)

# ZIP for easy distribution
$ZipPath = Join-Path $Root "release\Argos-v$Version-windows-x64.zip"
if (Test-Path $ZipPath) {
    Remove-Item $ZipPath -Force
}
Compress-Archive -Path (Join-Path $OutDir "*") -DestinationPath $ZipPath -CompressionLevel Optimal

Write-Host ""
Write-Host "Done."
Write-Host "  Folder: $OutDir"
Write-Host "  ZIP:    $ZipPath"
Get-ChildItem $OutDir -Recurse -File | ForEach-Object {
    $mb = [math]::Round($_.Length / 1MB, 2)
    $rel = $_.FullName.Substring($OutDir.Length + 1)
    Write-Host ("  - {0} ({1} MB)" -f $rel, $mb)
}
$zipMb = [math]::Round((Get-Item $ZipPath).Length / 1MB, 2)
Write-Host ("  ZIP size: {0} MB" -f $zipMb)
