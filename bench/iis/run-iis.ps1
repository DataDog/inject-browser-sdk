<#
.SYNOPSIS
  Accuracy check and throughput benchmark of the existing native IIS module (injector_IIS),
  with the same page set and checks as bench/run.sh does for nginx and httpd.

.DESCRIPTION
  NOT RUN BY THE AUTHOR (no Windows host was available): treat the first run as a shakedown.
  See bench/iis/README.md for prerequisites. Run from an elevated PowerShell, from the repo root:

    .\bench\iis\run-iis.ps1 -Oha C:\tools\oha.exe                    # accuracy + perf
    .\bench\iis\run-iis.ps1 -Mode accuracy
    .\bench\iis\run-iis.ps1 -Mode perf -Conns 16,64 -Duration 10s

  Steps:
   1. Generates the pages (bench/harness/gen_pages.py) into a new site directory.
   2. Creates an IIS site "rum-bench" on -Port, with URL Rewrite + ARR rules proxying /up/* to
      bench/harness/upstream.py (random-size writes, Content-Length or chunked).
   3. Module "on": <datadogRum enabled="true" version="6"> with the bench options in the site
      web.config. Module "off": the module is removed from the site (<modules><remove>).
   4. Accuracy: fetches a calibration page to learn the snippet IIS generates from that
      configuration, runs the oracle (Injector over each whole page, with that snippet), then
      bench/harness/accuracy.py over static, cl and chunked modes. IIS does not pad: when it does
      not inject it serves the body untouched, so every mode expects the unpadded output.
   5. Perf: oha over each perf page and connection count, off then on; req/s, p50, p99 and w3wp
      CPU per request, to results\perf-iis.jsonl and a markdown table (bench/harness/report.py).
#>
[CmdletBinding()]
param(
    [ValidateSet('all', 'accuracy', 'perf')] [string] $Mode = 'all',
    [int] $Port = 8080,
    [int] $UpstreamPort = 9000,
    [string] $SiteName = 'rum-bench',
    [string] $SiteRoot = 'C:\rum-bench',
    [string] $ModuleName = 'DDIISInjectorModule',
    [string] $Python = 'python',
    [string] $Oha = 'oha',
    # A prebuilt oracle.exe; when empty, `cargo run` builds it from bench/ (needs Rust + MSVC).
    [string] $Oracle = '',
    [int[]] $Conns = @(16, 64, 256),
    [string] $Duration = '10s',
    [int] $Seeds = 4,
    [string] $Results = ''
)
$ErrorActionPreference = 'Stop'
$bench = Split-Path -Parent $PSScriptRoot
$harness = Join-Path $bench 'harness'
if (-not $Results) { $Results = Join-Path $bench 'results' }
New-Item -ItemType Directory -Force -Path $Results | Out-Null
Import-Module WebAdministration

$pages = Join-Path $SiteRoot 'pages'
$expected = Join-Path $SiteRoot 'expected'
$appcmd = Join-Path $env:windir 'System32\inetsrv\appcmd.exe'

# The same options as bench/common SNIPPET_JSON, so IIS builds the same kind of snippet. The
# exact bytes are still learned from IIS (calibration), not assumed.
$rumOptions = @'
      <option name="applicationId" value="00000000-0000-0000-0000-000000000000" />
      <option name="clientToken" value="pub00000000000000000000000000000000" />
      <option name="site" value="datadoghq.com" />
      <option name="sessionSampleRate" value="100" />
'@

function Write-SiteConfig([string] $variant) {
    $rum = if ($variant -eq 'on') {
        "    <datadogRum enabled=`"true`" version=`"6`">`n      <clear />`n$rumOptions    </datadogRum>"
    } else {
        "    <modules>`n      <remove name=`"$ModuleName`" />`n    </modules>"
    }
    # /up/<mode>/<seed>/<path> -> upstream.py. Response buffering is governed by ARR's global
    # <proxy responseBufferLimit>, set to 0 below so upstream writes stream through.
    $config = @"
<?xml version="1.0" encoding="UTF-8"?>
<configuration>
  <system.webServer>
$rum
    <rewrite>
      <rules>
        <rule name="rum-bench-upstream" stopProcessing="true">
          <match url="^up/(.*)" />
          <action type="Rewrite" url="http://127.0.0.1:$UpstreamPort/{R:1}" />
        </rule>
      </rules>
    </rewrite>
    <staticContent>
      <clientCache cacheControlMode="NoControl" />
    </staticContent>
  </system.webServer>
</configuration>
"@
    Set-Content -Path (Join-Path $pages 'web.config') -Value $config -Encoding UTF8
    & $appcmd recycle apppool /apppool.name:$SiteName | Out-Null
    Start-Sleep -Seconds 2
    Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$Port/perf/html_2k.html" | Out-Null  # warm the worker
}

function Initialize-Site {
    if (-not (Get-Command $Python -ErrorAction SilentlyContinue)) { throw "python not found ($Python)" }
    if (-not (Test-Path "$env:windir\System32\inetsrv\rewrite.dll")) { throw 'IIS URL Rewrite is not installed' }
    if (-not (Get-WebGlobalModule -Name $ModuleName)) { throw "IIS module $ModuleName is not installed (install injector_IIS.msi)" }
    Remove-Item -Recurse -Force $SiteRoot -ErrorAction SilentlyContinue
    & $Python (Join-Path $harness 'gen_pages.py') $pages
    if ($LASTEXITCODE) { throw 'gen_pages.py failed' }
    Copy-Item (Join-Path $bench '..\lib\inject-browser-sdk\tests\data\regression\*.html') (Join-Path $pages 'corpus')
    Set-Content -Path (Join-Path $pages 'calibrate.html') -Value '<html><head></head><body></body></html>' -NoNewline -Encoding ascii

    # ARR: enable the proxy, stream responses (no buffering), keep the upstream's framing.
    & $appcmd set config -section:system.webServer/proxy /enabled:"True" /responseBufferLimit:"0" /preserveHostHeader:"False" /commit:apphost | Out-Null
    if (Get-Website -Name $SiteName) { Remove-Website -Name $SiteName }
    if (Test-Path "IIS:\AppPools\$SiteName") { Remove-WebAppPool -Name $SiteName }
    New-WebAppPool -Name $SiteName | Out-Null
    Set-ItemProperty "IIS:\AppPools\$SiteName" -Name managedRuntimeVersion -Value ''
    New-Website -Name $SiteName -Port $Port -IPAddress '127.0.0.1' -PhysicalPath $pages -ApplicationPool $SiteName | Out-Null
    # No dynamic/static compression: the module skips compressed bodies.
    & $appcmd set config $SiteName -section:system.webServer/urlCompression /doStaticCompression:"False" /doDynamicCompression:"False" /commit:apphost | Out-Null
}

function Start-Upstream {
    $script:upstream = Start-Process -PassThru -WindowStyle Hidden -FilePath $Python `
        -ArgumentList @((Join-Path $harness 'upstream.py'), $UpstreamPort, $pages)
    Start-Sleep -Seconds 1
}

function Get-RawResponse([string] $path) {
    $client = [System.Net.Sockets.TcpClient]::new('127.0.0.1', $Port)
    try {
        $stream = $client.GetStream()
        $request = [Text.Encoding]::ASCII.GetBytes("GET $path HTTP/1.1`r`nHost: bench`r`nConnection: close`r`n`r`n")
        $stream.Write($request, 0, $request.Length)
        $buffer = [IO.MemoryStream]::new()
        $stream.CopyTo($buffer)
        return $buffer.ToArray()
    } finally { $client.Dispose() }
}

function Invoke-Accuracy {
    Write-SiteConfig 'on'
    # Calibration: <html><head></head>... comes back as <html><head>SNIPPET</head>...
    $raw = Get-RawResponse '/calibrate.html'
    $text = [Text.Encoding]::GetEncoding('iso-8859-1').GetString($raw)
    $body = $text.Substring($text.IndexOf("`r`n`r`n") + 4)
    $prefix = '<html><head>'; $suffix = '</head><body></body></html>'
    if (-not ($body.StartsWith($prefix) -and $body.EndsWith($suffix)) -or $body.Length -le ($prefix.Length + $suffix.Length)) {
        throw "calibration page was not injected as expected:`n$text"
    }
    $snippet = $body.Substring($prefix.Length, $body.Length - $prefix.Length - $suffix.Length)
    $snippetFile = Join-Path $SiteRoot 'snippet.bin'
    [IO.File]::WriteAllBytes($snippetFile, [Text.Encoding]::GetEncoding('iso-8859-1').GetBytes($snippet))
    Write-Host "IIS snippet: $($snippet.Length) bytes"

    $pageFiles = @(Get-ChildItem (Join-Path $pages 'perf\*.html'), (Join-Path $pages 'corpus\*.html') | ForEach-Object FullName)
    if ($Oracle) {
        & $Oracle --snippet $snippetFile $expected @pageFiles
    } else {
        Push-Location $bench
        try { cargo run --release -q -p bench-tools --bin oracle -- --snippet $snippetFile $expected @pageFiles }
        finally { Pop-Location }
    }
    if ($LASTEXITCODE) { throw 'oracle failed' }

    & $Python (Join-Path $harness 'accuracy.py') --server iis/on --port $Port --pages $pages `
        --expected $expected --seeds $Seeds --modes 'static,cl,chunked' --expect unpadded `
        --out (Join-Path $Results 'accuracy-iis-on.json')
    $script:accuracyFailed = [bool]$LASTEXITCODE
}

function Get-W3wpCpuSeconds {
    $total = 0.0
    foreach ($p in Get-Process w3wp -ErrorAction SilentlyContinue) { $total += $p.TotalProcessorTime.TotalSeconds }
    return $total
}

function Invoke-Oha([string] $url, [int] $c, [string] $duration) {
    $json = & $Oha -z $duration -c $c --no-tui --output-format json $url 2>$null
    if ($LASTEXITCODE) { $json = & $Oha -z $duration -c $c --no-tui -j $url }  # older oha
    return ($json | Out-String | ConvertFrom-Json)
}

function Invoke-Perf {
    $out = Join-Path $Results 'perf-iis.jsonl'
    Remove-Item $out -ErrorAction SilentlyContinue
    $perfPages = Get-ChildItem (Join-Path $pages 'perf') | ForEach-Object Name
    foreach ($variant in 'off', 'on') {
        Write-SiteConfig $variant
        foreach ($page in $perfPages) {
            $url = "http://127.0.0.1:$Port/perf/$page"
            Invoke-Oha $url 16 '2s' | Out-Null  # warm-up
            foreach ($c in $Conns) {
                $before = Get-W3wpCpuSeconds
                $r = Invoke-Oha $url $c $Duration
                $cpu = (Get-W3wpCpuSeconds) - $before
                $requests = [int64]($r.statusCodeDistribution.'200')
                $errors = ($r.errorDistribution.PSObject.Properties | ForEach-Object { "$($_.Name): $($_.Value)" }) -join '; '
                $row = [ordered]@{
                    server = 'iis'; variant = $variant; page = $page; conns = $c
                    rps = [double]$r.summary.requestsPerSec; requests = $requests
                    p50_ms = 1000 * [double]$r.latencyPercentiles.p50
                    p75_ms = 1000 * [double]$r.latencyPercentiles.p75
                    p90_ms = 1000 * [double]$r.latencyPercentiles.p90
                    p99_ms = 1000 * [double]$r.latencyPercentiles.p99
                    socket_errors = $errors
                    non_2xx = [int64]($r.statusCodeDistribution.PSObject.Properties | Where-Object Name -ne '200' | Measure-Object Value -Sum).Sum
                    server_cpu_s = [math]::Round($cpu, 3)
                    # w3wp only: kernel time spent in http.sys is not attributed to it.
                    cpu_us_per_req = [math]::Round($cpu / [math]::Max($requests, 1) * 1e6, 2)
                }
                Write-Host ("iis {0,-3} {1,-24} c={2,-4} {3,10:N0} req/s  p50 {4:N2} ms  p99 {5:N2} ms  cpu {6:N1} us/req {7}" -f `
                    $variant, $page, $c, $row.rps, $row.p50_ms, $row.p99_ms, $row.cpu_us_per_req, $errors)
                Add-Content -Path $out -Value ($row | ConvertTo-Json -Compress)
            }
        }
    }
    & $Python (Join-Path $harness 'report.py') $out | Tee-Object -FilePath (Join-Path $Results 'perf-iis.md')
}

Initialize-Site
Start-Upstream
try {
    if ($Mode -in 'all', 'accuracy') { Invoke-Accuracy }
    if ($Mode -in 'all', 'perf') { Invoke-Perf }
} finally {
    if ($script:upstream) { Stop-Process -Id $script:upstream.Id -ErrorAction SilentlyContinue }
}
if ($script:accuracyFailed) { Write-Warning "accuracy mismatches: see $Results\accuracy-iis-on.json"; exit 1 }
