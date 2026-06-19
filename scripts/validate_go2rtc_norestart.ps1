# Headless validation for the go2rtc "no-restart on CRUD" change.
# Starts zoomy against an isolated temp data dir, then asserts that go2rtc's PID
# stays fixed across add/rename/delete (reconciled live) but DOES change on a
# same-name source edit (force-restart fallback). Run from the repo root.
#
# Exits non-zero if any assertion fails, so it is CI/automation-gatable.
$ErrorActionPreference = "Stop"
$env:LIBCLANG_PATH = "$env:APPDATA\Python\Python311\site-packages\clang\native"

$port = 8099
$go2rtcApi = "http://127.0.0.1:1984"
$base = "http://127.0.0.1:$port"
$data = Join-Path $env:TEMP "zoomy-validate-$port"
if (Test-Path $data) { Remove-Item $data -Recurse -Force }
New-Item -ItemType Directory -Force $data | Out-Null

$script:fails = 0
function Fail($m) { $script:fails++; Write-Output "  !! FAIL: $m" }
function Streams { (Invoke-RestMethod "$go2rtcApi/api/streams" -TimeoutSec 4).PSObject.Properties.Name | Sort-Object }
function Go2Pid { (Get-Process go2rtc -ErrorAction SilentlyContinue | Select-Object -First 1).Id }
function Cams { Invoke-RestMethod "$base/api/cameras" -TimeoutSec 4 }
# Poll until a stream is present/absent (reconcile + go2rtc are async, so fixed
# sleeps race on slow boxes). Returns when the predicate holds or times out.
function WaitStream($name, $present, $timeoutSec = 8) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        $has = (Streams) -contains $name
        if ($has -eq $present) { return $true }
        Start-Sleep -Milliseconds 300
    }
    return $false
}

# Preflight: we must own the only go2rtc, or PID/stream assertions are bogus
# (they would observe a foreign instance sharing the default :1984).
if (Get-Process go2rtc -ErrorAction SilentlyContinue) {
    Write-Output "ABORT: a go2rtc is already running; stop it so this validation watches the right instance."
    exit 2
}

Write-Output "Building + launching zoomy (port $port, data $data)..."
$srv = Start-Process -FilePath "cargo" -WorkingDirectory "E:\dev\ZoomyZoomyCamCam" `
    -ArgumentList @("run","-q","-p","zoomy","--","--port","$port","--data-dir","$data","--go2rtc-bin","E:\dev\ZoomyZoomyCamCam\bin\go2rtc.exe") `
    -PassThru -WindowStyle Hidden -RedirectStandardError "$data\stderr.log" -RedirectStandardOutput "$data\stdout.log"

try {
    # Wait for the API.
    $up = $false
    foreach ($i in 1..120) {
        try { Invoke-RestMethod "$base/api/cameras" -TimeoutSec 2 | Out-Null; $up = $true; break } catch { Start-Sleep -Milliseconds 750 }
    }
    if (-not $up) { throw "server never came up; see $data\stderr.log" }
    # go2rtc should be the single instance we just spawned.
    foreach ($i in 1..20) { if (Go2Pid) { break }; Start-Sleep -Milliseconds 500 }
    if ((Get-Process go2rtc -ErrorAction SilentlyContinue | Measure-Object).Count -ne 1) {
        Fail "expected exactly one go2rtc process to inspect"
    }
    $pid0 = Go2Pid
    Write-Output "PASS: server up. go2rtc pid=$pid0, streams=[$((Streams) -join ',')]"

    # 1) ADD — expect no restart, stream appears.
    Invoke-RestMethod "$base/api/cameras" -Method Post -ContentType "application/json" `
        -Body '{"name":"valcam","source":"rtsp://127.0.0.1:9999/dummy","detect":false,"record":false}' | Out-Null
    if (-not (WaitStream "valcam" $true)) { Fail "valcam stream did not appear after add" }
    $pidA = Go2Pid
    Write-Output "ADD: pid=$pidA streams=[$((Streams) -join ',')]  (restart=$($pidA -ne $pid0))"
    if ($pidA -ne $pid0) { Fail "go2rtc restarted on add" }

    # 2) RENAME — name-only reconcile (delete old + add new), no restart.
    $id = (Cams | Where-Object { $_.name -eq "valcam" }).id
    Invoke-RestMethod "$base/api/cameras/$id" -Method Patch -ContentType "application/json" -Body '{"name":"valcam2"}' | Out-Null
    if (-not (WaitStream "valcam2" $true)) { Fail "valcam2 stream did not appear after rename" }
    if (-not (WaitStream "valcam" $false)) { Fail "old valcam stream not removed after rename" }
    $pidR = Go2Pid
    Write-Output "RENAME: pid=$pidR streams=[$((Streams) -join ',')]  (restart=$($pidR -ne $pidA))"
    if ($pidR -ne $pidA) { Fail "go2rtc restarted on rename" }

    # 3) SOURCE EDIT (same name) — must force-restart so the new source takes.
    Invoke-RestMethod "$base/api/cameras/$id" -Method Patch -ContentType "application/json" -Body '{"source":"rtsp://127.0.0.1:9998/changed"}' | Out-Null
    # A restart spawns a new PID; poll briefly for it to differ.
    foreach ($i in 1..16) { if ((Go2Pid) -ne $pidR) { break }; Start-Sleep -Milliseconds 300 }
    $pidS = Go2Pid; $sS = Streams
    Write-Output "SRC-EDIT: pid=$pidS streams=[$($sS -join ',')]  (restart=$($pidS -ne $pidR))"
    if ($pidS -eq $pidR) { Fail "source edit did NOT restart (stale producer)" }
    if ($sS -notcontains "valcam2") { Fail "valcam2 missing after source edit" }

    # 4) DELETE — reconcile removes just this stream, no restart.
    Invoke-RestMethod "$base/api/cameras/$id" -Method Delete | Out-Null
    if (-not (WaitStream "valcam2" $false)) { Fail "stream not removed on delete" }
    $pidD = Go2Pid
    Write-Output "DELETE: pid=$pidD streams=[$((Streams) -join ',')]  (restart=$($pidD -ne $pidS))"
    if ($pidD -ne $pidS) { Fail "go2rtc restarted on delete" }

    if ($script:fails -eq 0) { Write-Output "DONE: all assertions passed." }
    else { Write-Output "DONE: $($script:fails) assertion(s) FAILED." }
}
finally {
    if ($srv -and -not $srv.HasExited) { Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue }
    Get-Process go2rtc,ffmpeg -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}
exit $script:fails
