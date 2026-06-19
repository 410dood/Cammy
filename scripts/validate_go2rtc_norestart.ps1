# Headless validation for the go2rtc "no-restart on CRUD" change.
# Starts zoomy against an isolated temp data dir, then asserts that go2rtc's PID
# stays fixed across add/rename/delete (reconciled live) but DOES change on a
# same-name source edit (force-restart fallback). Run from the repo root.
$ErrorActionPreference = "Stop"
$env:LIBCLANG_PATH = "$env:APPDATA\Python\Python311\site-packages\clang\native"

$port = 8099
$go2rtcApi = "http://127.0.0.1:1984"
$base = "http://127.0.0.1:$port"
$data = Join-Path $env:TEMP "zoomy-validate-$port"
if (Test-Path $data) { Remove-Item $data -Recurse -Force }
New-Item -ItemType Directory -Force $data | Out-Null

function Streams { (Invoke-RestMethod "$go2rtcApi/api/streams" -TimeoutSec 4).PSObject.Properties.Name | Sort-Object }
function Go2Pid { (Get-Process go2rtc -ErrorAction SilentlyContinue | Select-Object -First 1).Id }
function Cams { Invoke-RestMethod "$base/api/cameras" -TimeoutSec 4 }

Write-Output "Building + launching zoomy (port $port, data $data)..."
$srv = Start-Process -FilePath "cargo" -ArgumentList @("run","-q","-p","zoomy","--","--port","$port","--data-dir","$data") `
    -PassThru -WindowStyle Hidden -RedirectStandardError "$data\stderr.log" -RedirectStandardOutput "$data\stdout.log"

try {
    # Wait for the API.
    $up = $false
    foreach ($i in 1..120) {
        try { Invoke-RestMethod "$base/api/cameras" -TimeoutSec 2 | Out-Null; $up = $true; break } catch { Start-Sleep -Milliseconds 750 }
    }
    if (-not $up) { throw "server never came up; see $data\stderr.log" }
    Start-Sleep -Seconds 2  # let go2rtc settle
    $pid0 = Go2Pid
    Write-Output "PASS: server up. go2rtc pid=$pid0, streams=[$((Streams) -join ',')]"

    # 1) ADD — expect no restart, stream appears.
    Invoke-RestMethod "$base/api/cameras" -Method Post -ContentType "application/json" `
        -Body '{"name":"valcam","source":"rtsp://127.0.0.1:9999/dummy","detect":false,"record":false}' | Out-Null
    Start-Sleep -Seconds 1
    $pidA = Go2Pid; $sA = Streams
    Write-Output "ADD: pid=$pidA streams=[$($sA -join ',')]  (restart=$($pidA -ne $pid0))"
    if ($pidA -ne $pid0) { Write-Output "  !! FAIL: go2rtc restarted on add" }
    if ($sA -notcontains "valcam") { Write-Output "  !! FAIL: valcam stream missing" }

    # 2) RENAME — name-only reconcile (delete old + add new), no restart.
    $id = (Cams | Where-Object { $_.name -eq "valcam" }).id
    Invoke-RestMethod "$base/api/cameras/$id" -Method Patch -ContentType "application/json" -Body '{"name":"valcam2"}' | Out-Null
    Start-Sleep -Seconds 1
    $pidR = Go2Pid; $sR = Streams
    Write-Output "RENAME: pid=$pidR streams=[$($sR -join ',')]  (restart=$($pidR -ne $pidA))"
    if ($pidR -ne $pidA) { Write-Output "  !! FAIL: go2rtc restarted on rename" }
    if ($sR -contains "valcam" -or $sR -notcontains "valcam2") { Write-Output "  !! FAIL: rename not reflected in streams" }

    # 3) SOURCE EDIT (same name) — must force-restart so the new source takes.
    Invoke-RestMethod "$base/api/cameras/$id" -Method Patch -ContentType "application/json" -Body '{"source":"rtsp://127.0.0.1:9998/changed"}' | Out-Null
    Start-Sleep -Seconds 2
    $pidS = Go2Pid; $sS = Streams
    Write-Output "SRC-EDIT: pid=$pidS streams=[$($sS -join ',')]  (restart=$($pidS -ne $pidR))"
    if ($pidS -eq $pidR) { Write-Output "  !! FAIL: source edit did NOT restart (stale producer)" }
    if ($sS -notcontains "valcam2") { Write-Output "  !! FAIL: valcam2 missing after source edit" }

    # 4) DELETE — reconcile removes just this stream, no restart.
    Invoke-RestMethod "$base/api/cameras/$id" -Method Delete | Out-Null
    Start-Sleep -Seconds 1
    $pidD = Go2Pid; $sD = Streams
    Write-Output "DELETE: pid=$pidD streams=[$($sD -join ',')]  (restart=$($pidD -ne $pidS))"
    if ($pidD -ne $pidS) { Write-Output "  !! FAIL: go2rtc restarted on delete" }
    if ($sD -contains "valcam2") { Write-Output "  !! FAIL: stream not removed on delete" }

    Write-Output "DONE."
}
finally {
    if ($srv -and -not $srv.HasExited) { Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue }
    Get-Process go2rtc,ffmpeg -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}
