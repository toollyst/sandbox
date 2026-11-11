# install.ps1의 확인 분기를 가짜 릴리스(로컬 디렉터리)와 가짜 cosign으로 돌려 본다. 네트워크도 진짜 cosign도
# 쓰지 않는다. 묶음에는 진짜 sbx.exe와 sbx-shim.exe를 넣는다(설치 스크립트가 `sbx --version`을 돌린다).
# ci.yml(test, Windows)이 `cargo build -p sbx-cli -p sbx-shim` 뒤에 돌린다. 저장소 루트에서:
#
#   pwsh .github/scripts/test-install.ps1 [-Bin target\debug] [-Shell powershell.exe]
#
# -Shell: 설치 스크립트를 돌릴 PowerShell. 기본은 이 pwsh, `powershell.exe`면 Windows PowerShell 5.1.
param([string]$Bin = 'target\debug', [string]$Shell = '')
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$installer = Join-Path $root 'install.ps1'
$bin = (Resolve-Path (Join-Path $root $Bin)).Path
$pwsh = if ($Shell) { (Get-Command $Shell).Source } else { Join-Path $PSHOME 'pwsh.exe' }
Write-Host "install.ps1 under $pwsh"
$work = Join-Path ([IO.Path]::GetTempPath()) ("sbx-test-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$name = 'sbx-windows-x86_64'
$asset = "$name.zip"
$rel = Join-Path $work 'rel'

# 가짜 릴리스: sbx-windows-x86_64\{sbx.exe, sbx-shim.exe}.zip과 .sha256.
function New-Release([switch]$NoShim) {
    $pkg = Join-Path $work "pkg\$name"
    Remove-Item -Recurse -Force (Join-Path $work 'pkg') -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $pkg, $rel | Out-Null
    Copy-Item (Join-Path $bin 'sbx.exe') $pkg
    if (-not $NoShim) { Copy-Item (Join-Path $bin 'sbx-shim.exe') $pkg }
    Remove-Item (Join-Path $rel $asset) -ErrorAction SilentlyContinue
    Compress-Archive -Path $pkg -DestinationPath (Join-Path $rel $asset)
    $h = (Get-FileHash -Algorithm SHA256 (Join-Path $rel $asset)).Hash.ToLowerInvariant()
    Set-Content -NoNewline -Path (Join-Path $rel "$asset.sha256") -Value "$h  $asset`n"
}

# 가짜 cosign: 받은 인자를 한 줄에 하나씩 적고 FAKE_COSIGN_EXIT로 끝난다.
$cosignDir = Join-Path $work 'cosign-bin'
New-Item -ItemType Directory -Path $cosignDir | Out-Null
Set-Content -Path (Join-Path $cosignDir 'cosign.ps1') -Value @'
$args | Set-Content -Path $env:FAKE_COSIGN_ARGS
exit [int]("0" + $env:FAKE_COSIGN_EXIT)
'@
$cosignArgs = Join-Path $work 'cosign-args'
# cosign이 없는 PATH(이 머신에 진짜 cosign이 그 안에 있으면 "없음" 경우는 건너뛴다).
$plain = "$env:SystemRoot\System32;$env:SystemRoot"
$savedPath = $env:PATH
$env:PATH = $plain
$noCosign = $null -eq (Get-Command cosign -ErrorAction SilentlyContinue)
$env:PATH = $savedPath
$with = "$cosignDir;$plain"

$script:pass = 0
# Run <이름> <ok|fail> <출력에 있어야 할 글> <PATH> @{환경}
function Run([string]$Case, [string]$Want, [string]$Needle, [string]$Path, [hashtable]$Env = @{}) {
    $dir = Join-Path $work "install-$($script:pass)"
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
    $vars = @{ PATH = $Path; SBX_DOWNLOAD_URL = $rel; SBX_INSTALL_DIR = $dir; FAKE_COSIGN_ARGS = $cosignArgs;
        SBX_VERIFY = $null; SBX_VERSION = $null; FAKE_COSIGN_EXIT = $null }
    foreach ($k in $Env.Keys) { $vars[$k] = $Env[$k] }
    $saved = @{}
    foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
    try {
        $out = & $pwsh -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer 2>&1 | Out-String
        $code = $LASTEXITCODE
    } finally {
        foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
    }
    $got = if ($code -eq 0) { 'ok' } else { 'fail' }
    if ($got -ne $Want -or -not $out.Contains($Needle)) {
        Write-Host "FAIL ${Case}: wanted $Want with '$Needle', got $got (exit $code):"; Write-Host $out; exit 1
    }
    if ($Want -eq 'ok') {
        $v = & (Join-Path $dir 'sbx.exe') --version
        if ($LASTEXITCODE -ne 0 -or $v -notmatch '^sbx ') { Write-Host "FAIL ${Case}: sbx.exe not installed"; exit 1 }
        if (-not (Test-Path (Join-Path $dir 'sbx-shim.exe'))) { Write-Host "FAIL ${Case}: sbx-shim.exe not installed"; exit 1 }
        if (Get-ChildItem $dir -Filter '*.new') { Write-Host "FAIL ${Case}: left a .new file"; exit 1 }
    } elseif (Test-Path (Join-Path $dir 'sbx.exe')) {
        Write-Host "FAIL ${Case}: installed despite failing"; exit 1
    }
    $script:pass++
    Write-Host "ok   $Case"
}

try {
    New-Release
    if ($noCosign) {
        Run 'no cosign, auto: checksum only' ok 'only the SHA-256 was checked' $plain
        Run 'no cosign, require: refused' fail 'needs cosign' $plain @{ SBX_VERIFY = 'require' }
    }
    Run 'off: no signature check' ok 'installed sbx' $with @{ SBX_VERIFY = 'off'; FAKE_COSIGN_EXIT = '1' }
    Run 'bad SBX_VERIFY value' fail 'must be auto, require or off' $with @{ SBX_VERIFY = 'yes' }
    # cosign이 있으면 서명이 없는 것은 오류다(URL을 쥔 쪽이 서명만 지울 수 있다).
    Run 'auto, signature removed: refused' fail 'no signature' $with
    Run 'auto, unsigned release by name: refused' fail 'no signature' $with @{ SBX_VERSION = 'v0.2.0' }
    Run 'bad SBX_VERSION: refused' fail 'must be a release tag' $with @{ SBX_VERSION = 'v1;rm x' }
    Run 'bad SBX_VERSION (not a tag): refused' fail 'must be a release tag' $with @{ SBX_VERSION = 'latest' }
    Set-Content -Path (Join-Path $rel "$asset.sigstore.json") -Value '{}'
    Run 'auto, signature that does not verify: refused' fail 'does not verify' $with @{ FAKE_COSIGN_EXIT = '1' }
    Run 'require, signature that does not verify: refused' fail 'does not verify' $with @{ SBX_VERIFY = 'require'; FAKE_COSIGN_EXIT = '1' }
    Run 'auto, good signature' ok 'verified the Sigstore signature' $with
    # cosign에 넘긴 신원: 이 저장소의 release.yml, v* 태그, GitHub Actions 발급자.
    $a = Get-Content $cosignArgs
    if ($a -notcontains '^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/v') { Write-Host "FAIL: cosign identity regexp: $a"; exit 1 }
    if ($a -notcontains 'https://token.actions.githubusercontent.com') { Write-Host "FAIL: cosign issuer: $a"; exit 1 }
    if ($a -notcontains '--certificate-identity-regexp') { Write-Host "FAIL: cosign flags: $a"; exit 1 }
    Run 'require, good signature' ok 'verified the Sigstore signature' $with @{ SBX_VERIFY = 'require' }
    # 버전을 고르면 그 태그에서 온 서명이어야 한다. 정규식의 . 과 +는 글자 그대로다.
    Run 'by name: the exact tag' ok 'verified the Sigstore signature' $with @{ SBX_VERSION = 'v0.2.0+build.1' }
    $a = Get-Content $cosignArgs
    if ($a -notcontains '^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/v0[.]2[.]0[+]build[.]1$') { Write-Host "FAIL: exact tag regexp: $a"; exit 1 }
    # 이미 설치된 것 위에 다시 설치(업그레이드).
    $again = Join-Path $work 'install-again'
    foreach ($i in 1, 2) {
        [Environment]::SetEnvironmentVariable('SBX_INSTALL_DIR', $again); [Environment]::SetEnvironmentVariable('SBX_DOWNLOAD_URL', $rel)
        [Environment]::SetEnvironmentVariable('SBX_VERIFY', 'off')
        & $pwsh -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer *> $null
        if ($LASTEXITCODE -ne 0) { Write-Host "FAIL: install #$i over an existing one"; exit 1 }
    }
    'SBX_INSTALL_DIR', 'SBX_DOWNLOAD_URL', 'SBX_VERIFY' | ForEach-Object { [Environment]::SetEnvironmentVariable($_, $null) }
    $script:pass++; Write-Host 'ok   installs over an existing install'

    # 체크섬이 틀리면(서명보다 먼저) 거부한다.
    Set-Content -NoNewline -Path (Join-Path $rel "$asset.sha256") -Value ("0" * 64 + "  $asset`n")
    Run 'checksum mismatch: refused' fail 'checksum mismatch' $with @{ SBX_VERIFY = 'off' }
    Remove-Item (Join-Path $rel "$asset.sha256")
    Run 'missing checksum file: refused' fail 'download failed' $with @{ SBX_VERIFY = 'off' }
    # 묶음에 sbx-shim.exe가 없으면 거부한다.
    New-Release -NoShim
    Run 'archive without sbx-shim.exe: refused' fail 'does not contain sbx.exe and sbx-shim.exe' $with @{ SBX_VERIFY = 'off' }
    Write-Host "install.ps1: $($script:pass) cases passed ($pwsh)"
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
