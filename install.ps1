# Install sbx on Windows.
#
#   irm https://toollyst.github.io/sandbox/install.ps1 | iex
#
# Environment:
#   SBX_VERSION       release tag to install (default: the latest release), e.g. v0.3.0
#   SBX_INSTALL_DIR   where sbx.exe and sbx-shim.exe go (default: %LOCALAPPDATA%\Programs\sbx)
#   SBX_DOWNLOAD_URL  where the release files are (default: GitHub Releases of toollyst/sandbox)
#   SBX_VERIFY        auto (default), require or off: check the release's Sigstore signature with cosign.
#                     auto checks it when cosign is on PATH (and then a missing signature is an error);
#                     require also fails without cosign. With SBX_VERSION, the signature must come from that exact tag.
#
# What it does: downloads sbx-windows-<arch>.zip and its .sha256, checks the checksum and (see SBX_VERIFY) the
# Sigstore signature that release.yml made for it, and copies sbx.exe and sbx-shim.exe into SBX_INSTALL_DIR. It does
# not install an executor and does not change PATH; it prints what to do next. Nothing is written outside
# SBX_INSTALL_DIR, and it needs no administrator rights. The same steps as install.sh, for PowerShell 5.1 and 7.

function Install-Sbx {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $repo = 'toollyst/sandbox'
    function Say([string]$m) { Write-Host "sbx-install: $m" }
    # throw, not exit: under `irm | iex` exit would close the user's shell.
    function Fail([string]$m) { throw "sbx-install: error: $m" }

    switch ($env:PROCESSOR_ARCHITECTURE) {
        'AMD64' { $arch = 'x86_64' }
        default { Fail "unsupported architecture $($env:PROCESSOR_ARCHITECTURE) (x86_64 is supported)" }
    }
    $version = $env:SBX_VERSION
    if ($version -and $version -notmatch '^v[0-9][0-9A-Za-z.+-]*$') {
        Fail "SBX_VERSION must be a release tag like v0.3.0 (got $version)"
    }
    if ($env:SBX_DOWNLOAD_URL) { $base = $env:SBX_DOWNLOAD_URL.TrimEnd('/') }
    elseif ($version) { $base = "https://github.com/$repo/releases/download/$version" }
    else { $base = "https://github.com/$repo/releases/latest/download" }
    $dir = $env:SBX_INSTALL_DIR
    if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'Programs\sbx' }
    $verify = $env:SBX_VERIFY
    if (-not $verify) { $verify = 'auto' }
    if ($verify -notin @('auto', 'require', 'off')) { Fail "SBX_VERIFY must be auto, require or off (got $verify)" }
    $name = "sbx-windows-$arch"
    $asset = "$name.zip"

    # PowerShell 5.1 does not offer TLS 1.2 by default.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    # A file:// or plain local base is copied (tests and mirrors on disk); anything else is downloaded.
    function Get-File([string]$url, [string]$out) {
        if ($url -match '^file://') {
            Copy-Item -LiteralPath ([Uri]$url).LocalPath -Destination $out
        } elseif ($url -notmatch '^[a-z]+://') {
            Copy-Item -LiteralPath $url -Destination $out
        } else {
            Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $out
        }
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("sbx-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Say "downloading $base/$asset"
        try { Get-File "$base/$asset" "$tmp\$asset" } catch { Fail "download failed: $base/$asset" }
        try { Get-File "$base/$asset.sha256" "$tmp\$asset.sha256" } catch { Fail "download failed: $base/$asset.sha256" }
        $want = ((Get-Content -LiteralPath "$tmp\$asset.sha256" -TotalCount 1) -split '\s+')[0].ToLowerInvariant()
        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath "$tmp\$asset").Hash.ToLowerInvariant()
        if ($want -ne $got) { Fail "checksum mismatch for $asset (expected $want, got $got)" }

        # The signature says the archive was built by this repository's release workflow from a v* tag (exactly
        # SBX_VERSION when given). The checksum above only says the download is complete.
        if ($verify -ne 'off') {
            $cosign = Get-Command cosign -ErrorAction SilentlyContinue
            if (-not $cosign) {
                if ($verify -eq 'require') { Fail 'SBX_VERIFY=require needs cosign (https://docs.sigstore.dev/cosign/system_config/installation/)' }
                Say 'note: cosign is not on PATH, so only the SHA-256 was checked. To check the signature too, install cosign, or'
                Say "      run: gh attestation verify $asset -R $repo"
            } else {
                # Whoever controls the download URL can delete the signature, so with cosign at hand a missing one is an error.
                try { Get-File "$base/$asset.sigstore.json" "$tmp\$asset.sigstore.json" }
                catch { Fail "no signature for $asset at $base (set SBX_VERIFY=off to install without checking it)" }
                $tag = 'v'
                if ($version) { $tag = ($version -replace '\.', '[.]' -replace '\+', '[+]') + '$' }
                $identity = "^https://github[.]com/$repo/[.]github/workflows/release[.]yml@refs/tags/$tag"
                & $cosign.Source verify-blob --bundle "$tmp\$asset.sigstore.json" --certificate-identity-regexp $identity `
                    --certificate-oidc-issuer https://token.actions.githubusercontent.com "$tmp\$asset" *> $null
                if ($LASTEXITCODE -ne 0) { Fail "the Sigstore signature of $asset does not verify" }
                Say "verified the Sigstore signature (built by $repo release.yml from a v* tag)"
            }
        }

        Expand-Archive -LiteralPath "$tmp\$asset" -DestinationPath "$tmp\x"
        $from = Join-Path "$tmp\x" $name
        foreach ($f in 'sbx.exe', 'sbx-shim.exe') {
            if (-not (Test-Path -LiteralPath (Join-Path $from $f) -PathType Leaf)) { Fail "$asset does not contain sbx.exe and sbx-shim.exe" }
        }
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        # Copy next to the target, then replace it, so a failed copy never leaves half a binary behind.
        foreach ($f in 'sbx.exe', 'sbx-shim.exe') {
            Copy-Item -LiteralPath (Join-Path $from $f) -Destination (Join-Path $dir "$f.new") -Force
        }
        foreach ($f in 'sbx.exe', 'sbx-shim.exe') {
            try { Move-Item -LiteralPath (Join-Path $dir "$f.new") -Destination (Join-Path $dir $f) -Force }
            catch {
                Remove-Item -LiteralPath (Join-Path $dir "$f.new") -Force -ErrorAction SilentlyContinue
                Fail "cannot replace $(Join-Path $dir $f); is sbx running? ($($_.Exception.Message))"
            }
        }
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    $sbx = Join-Path $dir 'sbx.exe'
    $v = & $sbx --version
    if ($LASTEXITCODE -ne 0) { Fail "$sbx does not run" }
    Say "installed $v to $dir"
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') -contains $dir) -and -not (($env:Path -split ';') -contains $dir)) {
        Say "note: $dir is not on your PATH. Add it for your user (then open a new terminal):"
        Say "      [Environment]::SetEnvironmentVariable('Path', `"$dir;`" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')"
    }
    Say 'done. sbx on Windows can pull, prepare and list images. There is no tested executor for Windows yet, so it'
    Say 'cannot run sandboxes; see what your repositories offer for this host with:'
    Say "    $sbx executor search"
}

Install-Sbx
