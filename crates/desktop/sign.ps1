# Windows Authenticode signing hook, invoked by the Tauri bundler for every
# artifact (exe, NSIS installer). Deliberately a NO-OP unless a certificate is
# configured, so unsigned local/CI builds always succeed.
#
# The owner supplies ONE of:
#   CAMMY_SIGN_THUMBPRINT  - thumbprint of a code-signing cert in the CurrentUser
#                            or LocalMachine "My" store (classic OV/EV cert).
#   CAMMY_SIGN_COMMAND     - a full custom command to run; "%1" is replaced with
#                            the artifact path (e.g. an Azure Trusted Signing
#                            `signtool ... /dlib ...` invocation).
#
# Nothing here embeds or fabricates a certificate. Without one, installs show
# the SmartScreen "unknown publisher" prompt — signing removes it.
param([Parameter(Mandatory = $true)][string]$Path)

$ErrorActionPreference = "Stop"

if ($env:CAMMY_SIGN_COMMAND) {
    $cmd = $env:CAMMY_SIGN_COMMAND -replace "%1", ('"' + $Path + '"')
    Write-Host "cammy sign: running custom sign command for $Path"
    cmd /c $cmd
    if ($LASTEXITCODE -ne 0) { throw "custom sign command failed ($LASTEXITCODE)" }
    exit 0
}

if ($env:CAMMY_SIGN_THUMBPRINT) {
    $signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if (-not $signtool) {
        # Fall back to the newest Windows SDK signtool.
        $candidates = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue | Sort-Object FullName -Descending
        if ($candidates) { $signtool = $candidates[0].FullName } else { throw "signtool.exe not found (install a Windows SDK)" }
    } else {
        $signtool = $signtool.Source
    }
    Write-Host "cammy sign: signing $Path with cert $($env:CAMMY_SIGN_THUMBPRINT)"
    & $signtool sign /sha1 $env:CAMMY_SIGN_THUMBPRINT /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $Path
    if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
    exit 0
}

Write-Host "cammy sign: no CAMMY_SIGN_THUMBPRINT / CAMMY_SIGN_COMMAND set - leaving $Path unsigned"
exit 0
