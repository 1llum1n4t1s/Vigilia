<#
.SYNOPSIS
Install Vigilia (vigilia.exe) from the Vigilia download site.

.DESCRIPTION
Downloads the Windows x64 release zip from https://vigilia.kagayoi.com (Cloudflare R2; GitHub Releases are not used),
checks its SHA-256 against the published checksum, checks that vigilia.exe carries a valid Authenticode
signature from the Vigilia publisher, then installs it and adds the folder to the user PATH.

Layout of the download site:
  latest.json                                              {"schema","version","targets":{<target>:{"archive","checksum","sha256"}}}
  <ver>/vigilia-<ver>-x86_64-pc-windows-msvc.zip           immutable
  <ver>/vigilia-<ver>-x86_64-pc-windows-msvc.zip.sha256    immutable

Latest release:
  irm https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.ps1 | iex

A specific release (arguments cannot be passed through "| iex", so use a script block):
  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.ps1))) -Version 1.0.1

Works in Windows PowerShell 5.1 and PowerShell 7. macOS / Linux: use install.sh.

NOTE: this file is deliberately ASCII-only (comments and strings in English). It is saved to disk and run by
Windows PowerShell 5.1, which reads a BOM-less file as the ANSI code page; non-ASCII text can swallow line breaks.

.PARAMETER Version
Release to install, e.g. 1.0.0. Default: the newest release (read from latest.json).

.PARAMETER BaseUrl
Download site. Default: https://vigilia.kagayoi.com. Must be https (plain http only for a loopback
address, so that tests can use a local mock server). The signature check below always applies.

.PARAMETER InstallDir
Default: %LOCALAPPDATA%\Programs\Vigilia

.PARAMETER NoPath
Do not add InstallDir to the user PATH.

.PARAMETER ZipPath
Install from a local release zip instead of downloading (the signature is still checked).

.PARAMETER KeepDownload
Keep the temporary download folder (for troubleshooting).
#>
[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [string]$BaseUrl = 'https://vigilia.kagayoi.com',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\Vigilia'),
    [switch]$NoPath,
    [string]$ZipPath,
    [switch]$KeepDownload
)

# The whole body runs in a child scope. When this file is run through "irm | iex", StrictMode, $ErrorActionPreference
# and the helper variables below therefore do not leak into the caller's session.
& {
    # Expected signer: the CN of the Certum code-signing certificate. Matched by CN so that a certificate renewal
    # (new thumbprint) does not break installs. Downloads come only from R2 (-BaseUrl), the single distribution entry.
    $ExpectedSigner = 'Open Source Developer Yuichiro Shinozaki'
    $Target = 'x86_64-pc-windows-msvc'
    $Binaries = @('vigilia.exe')

    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version 2

    function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }

    if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'install.ps1 is for Windows. On macOS / Linux use install.sh.' }
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'Vigilia requires 64-bit Windows.' }
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') {
        Write-Warning 'Only an x64 build is published. It runs under emulation on Windows on ARM.'
    }
    # Windows PowerShell 5.1 may default to TLS 1.0, so enable TLS 1.2.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $work = Join-Path ([IO.Path]::GetTempPath()) ('vigilia-install-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        if ($ZipPath) {
            $zip = (Resolve-Path -LiteralPath $ZipPath).ProviderPath
            Write-Step "Using local package $zip"
        } else {
            # https only (plain http only for loopback, for tests). Redirects are never followed.
            $base = $BaseUrl.TrimEnd('/')
            $baseUri = [uri]$base
            if ($baseUri.Scheme -ne 'https' -and -not ($baseUri.Scheme -eq 'http' -and $baseUri.IsLoopback)) { throw "BaseUrl must be https: $BaseUrl" }
            $web = @{ UseBasicParsing = $true; MaximumRedirection = 0; Headers = @{ 'User-Agent' = 'vigilia-installer' } }

            $want = $Version.TrimStart('v')
            $latest = $null
            if ($want -eq 'latest') {
                Write-Step 'Looking up the latest release'
                $feed = (Invoke-WebRequest -Uri "$base/latest.json" @web).Content
                if ($feed -is [byte[]]) { $feed = [Text.Encoding]::UTF8.GetString($feed) }
                $latest = $feed | ConvertFrom-Json
                $want = [string]$latest.version
            }
            # The version must be x.y.z. The download keys are built from it; values in the feed are only
            # compared against them, so the feed can never redirect the installer to an arbitrary path.
            if ($want -notmatch '^\d+\.\d+\.\d+$') { throw "Invalid version '$want'." }
            $zipName = "vigilia-$want-$Target.zip"
            $zipKey = "$want/$zipName"
            $feedHash = $null
            if ($latest) {
                $entry = $null
                if ($latest.PSObject.Properties.Name -contains 'targets' -and $latest.targets.PSObject.Properties.Name -contains $Target) { $entry = $latest.targets.$Target }
                if (-not $entry -or [string]$entry.archive -cne $zipKey -or [string]$entry.checksum -cne "$zipKey.sha256") {
                    throw "latest.json points to unexpected files for $want."
                }
                # The hash in the feed is also compared with the real hash of the downloaded file.
                $feedHash = ([string]$entry.sha256).ToLowerInvariant()
            }

            $zip = Join-Path $work $zipName
            $sum = "$zip.sha256"
            Write-Step "Downloading $zipName"
            Invoke-WebRequest -Uri "$base/$zipKey" -OutFile $zip @web
            Invoke-WebRequest -Uri "$base/$zipKey.sha256" -OutFile $sum @web

            $expected = ((Get-Content -LiteralPath $sum -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
            $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($expected -ne $actual) { throw "SHA-256 mismatch for $zipName (expected $expected, got $actual)." }
            if ($feedHash -and $feedHash -ne $actual) { throw "SHA-256 mismatch for $zipName against latest.json (expected $feedHash, got $actual)." }
            Write-Step 'Checksum OK'
        }

        $extract = Join-Path $work 'extract'
        Expand-Archive -LiteralPath $zip -DestinationPath $extract
        foreach ($name in $Binaries) {
            $file = Join-Path $extract $name
            if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "The package does not contain $name." }
            $sig = Get-AuthenticodeSignature -LiteralPath $file
            $signer = if ($sig.SignerCertificate) { $sig.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) } else { '' }
            if ($sig.Status -ne 'Valid' -or $signer -cne $ExpectedSigner) { throw "Untrusted signature on ${name}: status $($sig.Status), signer '$signer'." }
        }
        Write-Step "Signature OK ($ExpectedSigner)"

        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        # A running exe cannot be overwritten or deleted, but it can be renamed. Move the old one aside first,
        # put the new one in place, and treat a failure to delete the old file as a warning only.
        # Leftovers from earlier runs are cleaned up the same way (best effort).
        foreach ($old in @(Get-ChildItem -LiteralPath $InstallDir -Filter '*.old-*' -File -ErrorAction SilentlyContinue)) {
            try { Remove-Item -LiteralPath $old.FullName -Force -ErrorAction Stop } catch { }
        }
        foreach ($name in $Binaries) {
            $dest = Join-Path $InstallDir $name
            $aside = $null
            if (Test-Path -LiteralPath $dest -PathType Leaf) {
                $aside = "$dest.old-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
                Move-Item -LiteralPath $dest -Destination $aside -Force
            }
            try {
                Copy-Item -LiteralPath (Join-Path $extract $name) -Destination $dest -Force
            } catch {
                # Roll back so that a failed copy does not leave the user without a working vigilia.exe.
                if ($aside -and -not (Test-Path -LiteralPath $dest)) { Move-Item -LiteralPath $aside -Destination $dest -Force }
                throw
            }
            if ($aside) {
                try { Remove-Item -LiteralPath $aside -Force -ErrorAction Stop }
                catch { Write-Warning "Could not remove the previous $name ($aside); it is probably still running. It will be removed by the next install." }
            }
        }
        foreach ($name in @('README.md', 'README.ja.md', 'LICENSE')) {
            $src = Join-Path $extract $name
            if (Test-Path -LiteralPath $src) { Copy-Item -LiteralPath $src -Destination $InstallDir -Force }
        }
        Write-Step "Installed to $InstallDir"

        if (-not $NoPath) {
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $entries = @(if ($userPath) { $userPath -split ';' | Where-Object { $_ } })
            if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
                [Environment]::SetEnvironmentVariable('Path', (@($entries + $InstallDir) -join ';'), 'User')
                Write-Step 'Added to the user PATH (open a new terminal to use it)'
            }
            if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) { $env:Path = "$env:Path;$InstallDir" }
        }

        & (Join-Path $InstallDir 'vigilia.exe') --version
        Write-Host ''
        Write-Host 'Next: vigilia needs the GitHub CLI signed in (gh auth login) or GH_TOKEN / GITHUB_TOKEN set.' -ForegroundColor Green
        Write-Host '  vigilia budget'
        Write-Host '  vigilia watch HEAD'
    } finally {
        if ($KeepDownload) { Write-Host "Download kept at $work" }
        else { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
    }
}
