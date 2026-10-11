function Install-RustFrp {
    param(
        [Parameter(Mandatory=$true)][string]$Controller,
        [Parameter(Mandatory=$true)][string]$EnrollToken,
        [Parameter(Mandatory=$true)][string]$DownloadBase,
        [string]$Name = $env:COMPUTERNAME,
        [string]$TlsPin = ''
    )
    $ErrorActionPreference = 'Stop'
    if (([Uri]$DownloadBase).Scheme -ne 'https') {
        throw 'Downloads require HTTPS'
    }
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    switch ($arch) {
        'X64' { $arch = 'x86_64' }
        'Arm64' { $arch = 'aarch64' }
        default { throw 'Unsupported architecture' }
    }
    $asset = "rustfrp-agent-windows-$arch.exe"
    $dir = Join-Path $env:LOCALAPPDATA 'RustFRP'
    $bin = Join-Path $dir 'bin'
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    $temp = Join-Path $bin ([Guid]::NewGuid().ToString() + '.exe')
    try {
        $base = $DownloadBase.TrimEnd('/')
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $temp -MaximumRedirection 0 -TimeoutSec 300
        $expected = (Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -MaximumRedirection 0 -TimeoutSec 30).Content.Trim()
        if ($expected -notmatch '^[0-9a-fA-F]{64}$') { throw 'Invalid SHA256 manifest' }
        if ((Get-FileHash -Algorithm SHA256 -Path $temp).Hash -ne $expected) { throw 'SHA256 mismatch' }
        $agent = Join-Path $bin 'rustfrp-agent.exe'
        Move-Item -Force -Path $temp -Destination $agent
        $pinArgs = @()
        if ($TlsPin) { $pinArgs = @('--tls-pin', $TlsPin) }
        & $agent --mode client --controller $Controller --enroll-token $EnrollToken --name $Name --state-dir $dir @pinArgs
        if ($LASTEXITCODE -ne 0) { throw "Agent exited with code $LASTEXITCODE" }
    } finally {
        if (Test-Path $temp) { Remove-Item -Force $temp }
    }
}
