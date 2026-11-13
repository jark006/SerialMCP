param([string]$OutputDirectory = "target/dist", [string]$BinaryPath = "target/release/serial-mcp.exe")
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$binary = Join-Path $repo $BinaryPath
if (-not (Test-Path -LiteralPath $binary)) { throw "Run cargo build --release first." }
$versionText = & $binary --version
if ($LASTEXITCODE -ne 0 -or $versionText -notmatch '^serial-mcp (\d+\.\d+\.\d+)$') { throw "Unexpected executable version: $versionText" }
$version = $Matches[1]
$destination = [System.IO.Path]::GetFullPath((Join-Path $repo $OutputDirectory))
New-Item -ItemType Directory -Force -Path $destination | Out-Null
$staging = Join-Path $destination ("serial-mcp-$version-windows-x64-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $staging | Out-Null
Copy-Item -LiteralPath $binary -Destination $staging
Copy-Item -LiteralPath (Join-Path $repo "README.md"), (Join-Path $repo "LICENSE") -Destination $staging
Copy-Item -LiteralPath (Join-Path $repo "docs") -Destination $staging -Recurse
$archive = Join-Path $destination "serial-mcp-$version-windows-x64.zip"
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $archive -Force
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath "$archive.sha256" -Value "$hash  $(Split-Path -Leaf $archive)" -Encoding ascii
# Only remove this invocation's staging directory after checking the resolved boundary.
$resolvedStaging = (Resolve-Path -LiteralPath $staging).Path
if (-not $resolvedStaging.StartsWith($destination + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) { throw "Staging path escaped output directory." }
Remove-Item -LiteralPath $resolvedStaging -Recurse -Force
Write-Output $archive
Write-Output "$archive.sha256"
