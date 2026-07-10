$ErrorActionPreference = "Stop"

$version = "5.34.3.1"
$archiveName = "strawberry-perl-$version-64bit-portable.zip"
$downloadUrl = "https://github.com/StrawberryPerl/Perl-Dist-Strawberry/releases/download/sp5.34.3.1/$archiveName"
$expectedSha256 = "94d312ed536bb5bec8d4d8a069c19cf5f275364b94bb4dd93da1c1aa5ef7652a"
$toolsRoot = Join-Path $env:LOCALAPPDATA "WezTerm\build-tools"
$installRoot = Join-Path $toolsRoot "strawberry-perl-$version"
$perlExe = Join-Path $installRoot "perl\bin\perl.exe"
$archivePath = Join-Path $toolsRoot $archiveName

function Test-Perl([string] $Path) {
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    return $false
  }

  & $Path -MFindBin -e "exit 0"
  return $LASTEXITCODE -eq 0
}

function Get-Sha256([string] $Path) {
  $stream = [IO.File]::OpenRead($Path)
  try {
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
      return ([BitConverter]::ToString($sha256.ComputeHash($stream))).Replace("-", "").ToLowerInvariant()
    } finally {
      $sha256.Dispose()
    }
  } finally {
    $stream.Dispose()
  }
}

if (Test-Perl $perlExe) {
  if (Test-Path -LiteralPath $archivePath -PathType Leaf) {
    Remove-Item -LiteralPath $archivePath -Force
  }
  Write-Host "Using cached Strawberry Perl: $perlExe"
  exit 0
}

New-Item -ItemType Directory -Path $toolsRoot -Force | Out-Null

if (Test-Path -LiteralPath $archivePath -PathType Leaf) {
  $actualSha256 = Get-Sha256 $archivePath
  if ($actualSha256 -ne $expectedSha256) {
    Remove-Item -LiteralPath $archivePath -Force
  }
}

if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
  Write-Host "Downloading Strawberry Perl $version..."
  Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath
}

$actualSha256 = Get-Sha256 $archivePath
if ($actualSha256 -ne $expectedSha256) {
  throw "Strawberry Perl archive checksum mismatch. Expected $expectedSha256, got $actualSha256"
}

$extractRoot = Join-Path $toolsRoot "strawberry-perl-$version.extracting-$PID"
$safePrefix = $toolsRoot.TrimEnd('\') + '\'
if (-not $extractRoot.StartsWith($safePrefix, [StringComparison]::OrdinalIgnoreCase)) {
  throw "Refusing to extract outside the build-tools directory: $extractRoot"
}

try {
  if (Test-Path -LiteralPath $extractRoot) {
    Remove-Item -LiteralPath $extractRoot -Recurse -Force
  }

  Write-Host "Extracting Strawberry Perl to $installRoot..."
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  [IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $extractRoot)

  $extractedPerl = Join-Path $extractRoot "perl\bin\perl.exe"
  if (-not (Test-Perl $extractedPerl)) {
    throw "The extracted Strawberry Perl installation is invalid: $extractedPerl"
  }

  if (Test-Path -LiteralPath $installRoot) {
    if (-not $installRoot.StartsWith($safePrefix, [StringComparison]::OrdinalIgnoreCase)) {
      throw "Refusing to replace a directory outside the build-tools directory: $installRoot"
    }
    Remove-Item -LiteralPath $installRoot -Recurse -Force
  }

  Move-Item -LiteralPath $extractRoot -Destination $installRoot
} finally {
  if (Test-Path -LiteralPath $extractRoot) {
    Remove-Item -LiteralPath $extractRoot -Recurse -Force
  }
}

Remove-Item -LiteralPath $archivePath -Force
Write-Host "Strawberry Perl is ready: $perlExe"
