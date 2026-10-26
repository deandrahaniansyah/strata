# Builds strata.exe (icon embedded by build.rs) and zips it under dist\.
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = "Stop"

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$name = "strata-$Version-windows-x86_64"
Remove-Item -Recurse -Force dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "dist\$name" | Out-Null
Copy-Item target\release\strata.exe, README.md, LICENSE "dist\$name\"
Compress-Archive -Path "dist\$name\*" -DestinationPath "dist\$name.zip"
Remove-Item -Recurse -Force "dist\$name"
