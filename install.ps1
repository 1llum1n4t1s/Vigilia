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
Default (empty): %LOCALAPPDATA%\Programs\Vigilia, a folder dedicated to Vigilia, where README.md, README.ja.md and
LICENSE are copied too. Any other folder may be shared (for example an existing bin directory), so only vigilia.exe
is placed there (the documents are not copied) and only the leftovers named vigilia.exe.old-* are cleaned up.

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
    [string]$InstallDir = '',
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

    # --- User PATH helpers -------------------------------------------------------------------------------------
    # The user PATH lives in HKCU\Environment\Path. It is usually REG_EXPAND_SZ and may contain %VAR% references,
    # so it is read WITHOUT expanding, written back with its original value kind, and compared entry by entry
    # after expanding. [Environment]::GetEnvironmentVariable/SetEnvironmentVariable would flatten the value to an
    # expanded REG_SZ string. Get-UserPathUpdate is pure (no registry access); tests load it from this file.

    # Returns $null when $Dir is already on the path, else the new raw value and the registry value kind to use.
    function Get-UserPathUpdate([bool]$Exists, [string]$Raw, $Kind, [string]$Dir) {
        $string = [Microsoft.Win32.RegistryValueKind]::String
        $expand = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($Exists -and $Kind -ne $string -and $Kind -ne $expand) { throw "The user Path has an unexpected registry value type ($Kind)." }
        $want = [Environment]::ExpandEnvironmentVariables($Dir).Trim().TrimEnd('\')
        foreach ($entry in @($Raw -split ';')) {
            if ($entry -and [Environment]::ExpandEnvironmentVariables($entry).Trim().TrimEnd('\') -ieq $want) { return $null }
        }
        # Existing entries are kept verbatim (unexpanded); only a trailing separator is dropped before appending.
        $value = if ($Raw) { $Raw.TrimEnd(';') + ';' + $Dir } else { $Dir }
        # A missing value is created as REG_EXPAND_SZ, the kind Windows itself uses for the user Path.
        $newKind = if ($Exists) { $Kind } else { $expand }
        return [pscustomobject]@{ Value = $value; Kind = $newKind }
    }

    # Tells running programs (Explorer, new terminals) that the environment changed, as SetEnvironmentVariable did.
    function Send-EnvironmentChange {
        try {
            if (-not ('VigiliaInstall.NativeMethods' -as [type])) {
                Add-Type -Namespace VigiliaInstall -Name NativeMethods -MemberDefinition (@(
                    '[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]',
                    'public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);'
                ) -join "`n")
            }
            $result = [UIntPtr]::Zero
            # HWND_BROADCAST, WM_SETTINGCHANGE, lParam "Environment", SMTO_ABORTIFHUNG, 5 second timeout.
            [void][VigiliaInstall.NativeMethods]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
        } catch {
            Write-Warning "Could not notify running programs of the PATH change: $($_.Exception.Message)"
        }
    }

    # Adds $Dir to the user PATH in the registry. Returns $true when it was added, $false when already present.
    function Add-ToUserPath([string]$Dir) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
        try {
            $exists = @($key.GetValueNames()) -contains 'Path'
            $raw = ''
            $kind = $null
            if ($exists) {
                $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                $kind = $key.GetValueKind('Path')
            }
            $update = Get-UserPathUpdate $exists $raw $kind $Dir
            if (-not $update) { return $false }
            $key.SetValue('Path', $update.Value, $update.Kind)
        } finally {
            $key.Close()
        }
        Send-EnvironmentChange
        return $true
    }

    if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'install.ps1 is for Windows. On macOS / Linux use install.sh.' }
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'Vigilia requires 64-bit Windows.' }
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') {
        Write-Warning 'Only an x64 build is published. It runs under emulation on Windows on ARM.'
    }

    # -InstallDir is empty by default (so that "irm | iex", which cannot pass arguments, behaves the same as a
    # normal run). An empty value means the default folder, which is dedicated to Vigilia. Whether the resolved
    # folder is that default decides if the documents are copied (a folder given explicitly may be shared).
    # This runs after the platform checks, and tolerates an empty LOCALAPPDATA: an explicit -InstallDir still
    # works (no documents are copied then), while omitting it stops with a clear message.
    $defaultInstallDir = if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA 'Programs\Vigilia' } else { '' }
    if ([string]::IsNullOrWhiteSpace($InstallDir)) {
        if (-not $defaultInstallDir) { throw 'LOCALAPPDATA is not set, so the default install folder cannot be determined. Pass -InstallDir.' }
        $InstallDir = $defaultInstallDir
    }
    # Make the folder absolute against the PowerShell location, and accept only file system paths. A relative
    # path would otherwise resolve against the process directory, and a provider path such as HKCU:\ is not a folder.
    $provider = $null
    $drive = $null
    $InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir, [ref]$provider, [ref]$drive)
    if ($provider.Name -ne 'FileSystem' -or ($InstallDir -notmatch '^[A-Za-z]:\\' -and $InstallDir -notmatch '^\\\\')) {
        throw "InstallDir must be a file system path on a drive or a UNC path: $InstallDir"
    }
    $isDefaultInstallDir = [bool]$defaultInstallDir -and ([IO.Path]::GetFullPath($InstallDir).TrimEnd('\') -ieq [IO.Path]::GetFullPath($defaultInstallDir).TrimEnd('\'))
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
        # Leftovers from earlier runs are cleaned up the same way (best effort). Only names this script creates
        # ("<binary>.old-*") are touched, because InstallDir may be a shared folder holding unrelated *.old-* files.
        foreach ($name in $Binaries) {
            foreach ($old in @(Get-ChildItem -LiteralPath $InstallDir -Filter "$name.old-*" -File -ErrorAction SilentlyContinue)) {
                try { Remove-Item -LiteralPath $old.FullName -Force -ErrorAction Stop } catch { }
            }
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
        # The documents are copied only into the default, Vigilia-dedicated folder. Any other folder may be shared
        # (for example an existing bin directory), where overwriting README / LICENSE would be harmful.
        if ($isDefaultInstallDir) {
            foreach ($name in @('README.md', 'README.ja.md', 'LICENSE')) {
                $src = Join-Path $extract $name
                if (Test-Path -LiteralPath $src) { Copy-Item -LiteralPath $src -Destination $InstallDir -Force }
            }
        }
        Write-Step "Installed to $InstallDir"

        if (-not $NoPath) {
            # The binary is already in place, so a PATH problem (for example a Path value of an unexpected registry
            # type, which is left untouched) is only a warning: the install itself still succeeds.
            try {
                if (Add-ToUserPath $InstallDir) { Write-Step 'Added to the user PATH (open a new terminal to use it)' }
            } catch {
                Write-Warning "Installed, but the user PATH was not updated: $($_.Exception.Message) Add $InstallDir to PATH yourself."
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
