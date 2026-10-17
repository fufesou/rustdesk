param([Parameter(Mandatory = $true)][string]$Target)

$ErrorActionPreference = 'Stop'
$helperDirectory = Join-Path $env:RUNNER_TEMP 'rustdesk-installer-helper'
New-Item -ItemType Directory -Path $helperDirectory -Force | Out-Null
$helperPath = Join-Path $helperDirectory 'installer-helper.exe'

# Keep the compiler flags aligned with build_installer_helper() in build.rs.
rustc --crate-name rustdesk_installer_helper --edition=2021 --target $Target `
    -C opt-level=z -C target-feature=+crt-static -C strip=symbols `
    -C link-arg=/MANIFEST:EMBED -o $helperPath res/installer_helper.rs
if ($LASTEXITCODE -ne 0) { throw 'Installer helper compilation failed' }

python3 -m pip install requests
if ($LASTEXITCODE -ne 0) { throw 'Installing signing dependencies failed' }
python3 res/job.py sign_one_file $helperPath
if ($LASTEXITCODE -ne 0) { throw 'Installer helper signing failed' }

# The signing client can return success after a failed request. Only embed a
# verified signed EXE; signing the application later cannot sign embedded bytes.
$signature = Get-AuthenticodeSignature -LiteralPath $helperPath
if ($signature.Status -ne 'Valid') {
    throw "Installer helper signature is not valid: $($signature.Status)"
}
"RUSTDESK_INSTALLER_HELPER=$helperPath" |
    Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
