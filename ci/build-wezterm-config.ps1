$ErrorActionPreference = "Stop"
$configRoot = Join-Path $PSScriptRoot "..\wezterm_config\wezterm-gui"
$targetDir = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\target"))
$oldTargetDir = $env:CARGO_TARGET_DIR
$pushedLocation = $false

try {
  Push-Location $configRoot
  $pushedLocation = $true
  npm ci
  if ($LASTEXITCODE -ne 0) { throw "npm ci failed with exit code $LASTEXITCODE" }

  npm run build
  if ($LASTEXITCODE -ne 0) { throw "frontend build failed with exit code $LASTEXITCODE" }

  npm test
  if ($LASTEXITCODE -ne 0) { throw "frontend tests failed with exit code $LASTEXITCODE" }

  $env:CARGO_TARGET_DIR = $targetDir
  cargo test --manifest-path src-tauri\Cargo.toml --release --locked --features custom-protocol
  if ($LASTEXITCODE -ne 0) { throw "Rust tests failed with exit code $LASTEXITCODE" }

  cargo build --manifest-path src-tauri\Cargo.toml --release --locked --features custom-protocol
  if ($LASTEXITCODE -ne 0) { throw "Rust build failed with exit code $LASTEXITCODE" }

  $executable = Join-Path $targetDir "release\wezterm-config.exe"
  if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Expected configurator executable was not produced: $executable"
  }
} finally {
  $env:CARGO_TARGET_DIR = $oldTargetDir
  if ($pushedLocation) { Pop-Location }
}
