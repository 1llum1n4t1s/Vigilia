#requires -Version 7.2
<#
.SYNOPSIS
install.ps1 / install.sh（R2 からの取得）と scripts/Publish-R2.ps1（入力検査）をローカルのモック HTTP で確かめる。

.DESCRIPTION
本物の R2 にも Certum にも接続しない。モックは 127.0.0.1 の HttpListener（install.* はループバックだけ http を許す）。
モックの zip は署名されていないので、install.ps1 の正常系は「取得・SHA-256 照合を通り、署名の検査で止まる」ことで確かめる
（署名の検査を迂回する口は install.ps1 に無い）。install.sh は署名を見ない（macOS の ad-hoc 署名だけ）ため、
偽の tar.gz で最後まで通し、配置・実行まで確かめる（bash と tar がある環境だけ）。確かめること:
  - latest.json の解決と、版付きキー（<ver>/…、…​.sha256）の取得
  - -Version 指定では latest.json を読まない
  - .sha256 と本体が違えば SHA-256 不一致で失敗
  - latest.json が想定外のパスを指していれば失敗し、そのパスへは接続しない
  - 非ループバックの http は拒否
  - Publish-R2.ps1 -ValidateOnly: 正常系と、.sha256 不一致・名前違い・余計なファイルの失敗
#>
[CmdletBinding()]
param(
    [string]$WorkDirectory = (Join-Path ([IO.Path]::GetTempPath()) ('vigilia-install-test-' + [guid]::NewGuid().ToString('N'))),
    # 指定すると、終了時に作業フォルダーを残す（調査用）。既定ではこの実行で作ったフォルダーを後片付けする。
    [switch]$KeepWorkDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).ProviderPath
$installPs1 = Join-Path $repo 'install.ps1'
$installSh = Join-Path $repo 'install.sh'
$publish = Join-Path $repo 'scripts/Publish-R2.ps1'
# この実行で新規に作ったフォルダーだけを後片付けの対象にする（呼び出し側が用意済みのフォルダーは消さない）。
$createdWorkDirectory = -not (Test-Path -LiteralPath $WorkDirectory)
New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null

$failures = [System.Collections.Generic.List[string]]::new()
function Assert-That([bool]$Condition, [string]$Message) {
    if ($Condition) { Write-Host "  ok   $Message" } else { Write-Host "  FAIL $Message" -ForegroundColor Red; $failures.Add($Message) }
}

$targets = [ordered]@{
    'x86_64-pc-windows-msvc'    = 'zip'
    'aarch64-apple-darwin'      = 'tar.gz'
    'x86_64-unknown-linux-gnu'  = 'tar.gz'
    'aarch64-unknown-linux-gnu' = 'tar.gz'
}

# --- 偽の配布物 ---------------------------------------------------------------------------------
# 版ごとに 4 ターゲットのアーカイブと .sha256 を作る。中身は偽物（zip は署名なし、tar.gz の vigilia は版を出すだけのシェル）。
function New-FakeDistribution([string]$Version, [string]$Dir) {
    New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    $src = Join-Path $Dir "src-$Version"
    New-Item -ItemType Directory -Path $src -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $src 'vigilia'), "#!/bin/sh`necho 'vigilia $Version'`n", [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText((Join-Path $src 'vigilia.exe'), "fake exe $Version", [Text.UTF8Encoding]::new($false))
    foreach ($f in 'README.md', 'README.ja.md', 'LICENSE') { Set-Content -LiteralPath (Join-Path $src $f) -Value "fake $f" }
    $out = Join-Path $Dir "dist-$Version"
    New-Item -ItemType Directory -Path $out -Force | Out-Null
    foreach ($target in $targets.Keys) {
        $name = "vigilia-$Version-$target.$($targets[$target])"
        $archive = Join-Path $out $name
        if ($targets[$target] -eq 'zip') {
            Compress-Archive -LiteralPath (Join-Path $src 'vigilia.exe'), (Join-Path $src 'README.md'), (Join-Path $src 'README.ja.md'), (Join-Path $src 'LICENSE') -DestinationPath $archive -Force
        } else {
            # GNU tar（Git for Windows）は "C:" で始まるパスをリモートホストと解釈するため、相対パスで渡す。
            Push-Location -LiteralPath $src
            try {
                & tar -czf ([IO.Path]::GetRelativePath($src, $archive)) vigilia README.md README.ja.md LICENSE
                if ($LASTEXITCODE -ne 0) { throw 'tar failed' }
            } finally { Pop-Location }
        }
        $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $name" | Set-Content -LiteralPath "$archive.sha256" -Encoding ascii -NoNewline
    }
    return $out
}
# Publish-R2.ps1 と同じ形の latest.json を作る。
function New-Feed([string]$Version, [string]$Dist) {
    $t = [ordered]@{}
    foreach ($target in $targets.Keys) {
        $name = "vigilia-$Version-$target.$($targets[$target])"
        $t[$target] = [ordered]@{ archive = "$Version/$name"; checksum = "$Version/$name.sha256"; sha256 = (Get-FileHash -LiteralPath (Join-Path $Dist $name) -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
    return [Text.Encoding]::UTF8.GetBytes(([ordered]@{ schema = 1; version = $Version; targets = $t } | ConvertTo-Json -Depth 5 -Compress))
}

$dist = New-FakeDistribution '9.9.9' (Join-Path $WorkDirectory 'pkg')
$winName = 'vigilia-9.9.9-x86_64-pc-windows-msvc.zip'
$linuxName = 'vigilia-9.9.9-x86_64-unknown-linux-gnu.tar.gz'
$badSumBytes = [Text.Encoding]::ASCII.GetBytes(('0' * 64) + "  $winName")

# --- モックサーバー -----------------------------------------------------------------------------
$port = Get-Random -Minimum 20000 -Maximum 40000
$prefix = "http://127.0.0.1:$port/"
$routes = [hashtable]::Synchronized(@{})
$requests = [System.Collections.ArrayList]::Synchronized([System.Collections.ArrayList]::new())
$listener = [Net.HttpListener]::new()
$listener.Prefixes.Add($prefix)
$listener.Start()
$serverJob = Start-ThreadJob -ArgumentList $listener, $routes, $requests -ScriptBlock {
    param($listener, $routes, $requests)
    while ($listener.IsListening) {
        try { $ctx = $listener.GetContext() } catch { break }
        $path = $ctx.Request.Url.AbsolutePath
        [void]$requests.Add($path)
        if ($routes.ContainsKey($path)) {
            $bytes = [byte[]]$routes[$path]
            $ctx.Response.StatusCode = 200
            $ctx.Response.OutputStream.Write($bytes, 0, $bytes.Length)
        } else { $ctx.Response.StatusCode = 404 }
        $ctx.Response.Close()
    }
}

function Invoke-InstallPs1([string[]]$ExtraArgs) {
    $requests.Clear()
    $target = Join-Path $WorkDirectory ('install-' + [guid]::NewGuid().ToString('N'))
    $out = & pwsh -NoProfile -File $installPs1 -BaseUrl $prefix -InstallDir $target -NoPath @ExtraArgs 2>&1 | Out-String
    return [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = $out; Requests = @($requests.ToArray()) }
}

try {
    foreach ($target in $targets.Keys) {
        $name = "vigilia-9.9.9-$target.$($targets[$target])"
        $routes["/9.9.9/$name"] = [IO.File]::ReadAllBytes((Join-Path $dist $name))
        $routes["/9.9.9/$name.sha256"] = [IO.File]::ReadAllBytes((Join-Path $dist "$name.sha256"))
    }
    $routes['/latest.json'] = New-Feed '9.9.9' $dist
    $goodSum = $routes["/9.9.9/$winName.sha256"]

    if ($IsWindows) {
    # --- install.ps1: ユーザー PATH の合成（純関数。実レジストリには触れない） ---------------------------
    # Windows だけで実行する（RegistryValueKind と %VAR% 展開は Windows の挙動を前提にするため、macOS / Linux の CI では通さない）。
    # install.ps1 から Get-UserPathUpdate の定義だけを AST で取り出して実行する（スクリプト本体は走らせない）。
    Write-Host 'install.ps1: ユーザー PATH の合成（展開・種別保持・重複判定。レジストリは読み書きしない）'
    $psTokens = $null; $psErrors = $null
    $psAst = [Management.Automation.Language.Parser]::ParseFile($installPs1, [ref]$psTokens, [ref]$psErrors)
    # 構文エラーは 187 行以降の install.ps1 の全実行で、関数の欠落は次の StrictMode 下の .Extent 参照の例外で検出する。
    $pathFnAst = $psAst.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Get-UserPathUpdate' }, $true) | Select-Object -First 1
    $pathFnText = $pathFnAst.Extent.Text
    . ([scriptblock]::Create($pathFnText))
    $kExpand = [Microsoft.Win32.RegistryValueKind]::ExpandString
    $kString = [Microsoft.Win32.RegistryValueKind]::String
    $savedTestBin = $env:VIGILIA_TEST_BIN
    $env:VIGILIA_TEST_BIN = 'C:\VigiliaTest\bin'
    try {
        $u = Get-UserPathUpdate $true '%VIGILIA_TEST_BIN%;C:\Tools' $kExpand 'C:\New\Vigilia'
        Assert-That ($u.Value -ceq '%VIGILIA_TEST_BIN%;C:\Tools;C:\New\Vigilia') '既存の項目は展開せずそのまま残して末尾へ追加する'
        Assert-That ($u.Kind -eq $kExpand) '元の値種別（REG_EXPAND_SZ）を保つ'
        $u = Get-UserPathUpdate $true 'C:\Tools' $kString 'C:\New\Vigilia'
        Assert-That ($u.Kind -eq $kString -and $u.Value -ceq 'C:\Tools;C:\New\Vigilia') '元が REG_SZ なら REG_SZ のまま書き戻す'
        $u = Get-UserPathUpdate $false '' $null 'C:\New\Vigilia'
        Assert-That ($u.Value -ceq 'C:\New\Vigilia' -and $u.Kind -eq $kExpand) '値が無ければ REG_EXPAND_SZ で新規に作る'
        $u = Get-UserPathUpdate $true 'C:\A;' $kExpand 'C:\New\Vigilia'
        Assert-That ($u.Value -ceq 'C:\A;C:\New\Vigilia') '末尾の区切りは重ねない'
        $u = Get-UserPathUpdate $true '' $kExpand 'C:\New\Vigilia'
        Assert-That ($u.Value -ceq 'C:\New\Vigilia') '空の値には項目だけを入れる'
        Assert-That ($null -eq (Get-UserPathUpdate $true 'C:\Tools;C:\New\Vigilia\' $kExpand 'c:\new\VIGILIA')) '末尾の \ と大文字小文字の違いは重複とみなす'
        Assert-That ($null -eq (Get-UserPathUpdate $true '%VIGILIA_TEST_BIN%;C:\Tools' $kExpand 'c:\vigiliatest\BIN\')) '環境変数を展開した結果が同じなら重複とみなす（追加しない）'
        Assert-That ($null -eq (Get-UserPathUpdate $true 'C:\Tools;C:\VigiliaTest\bin' $kExpand '%VIGILIA_TEST_BIN%')) '追加する側が環境変数参照でも展開して比べる'
        $threw = $false
        try { Get-UserPathUpdate $true 'C:\Tools' ([Microsoft.Win32.RegistryValueKind]::DWord) 'C:\New' | Out-Null } catch { $threw = $true }
        Assert-That $threw '文字列型でない Path の値は壊さずに止まる'
    } finally {
        if ($null -eq $savedTestBin) { Remove-Item Env:VIGILIA_TEST_BIN -ErrorAction SilentlyContinue } else { $env:VIGILIA_TEST_BIN = $savedTestBin }
    }
    # Windows PowerShell 5.1 でも同じ結果になること（導入先は 5.1 のことが多い）。
    $ps51Path = Get-Command powershell.exe -ErrorAction SilentlyContinue
    if ($ps51Path -and $IsWindows) {
        $ps51Script = Join-Path $WorkDirectory 'ps51-path.ps1'
        $ps51Body = @(
            '$env:VIGILIA_TEST_BIN = ''C:\VigiliaTest\bin''',
            $pathFnText,
            '$u = Get-UserPathUpdate $true ''%VIGILIA_TEST_BIN%;C:\Tools'' ([Microsoft.Win32.RegistryValueKind]::ExpandString) ''C:\New\Vigilia''',
            'Write-Output ("add=" + $u.Value + "|" + $u.Kind)',
            '$d = Get-UserPathUpdate $true ''%VIGILIA_TEST_BIN%;C:\Tools'' ([Microsoft.Win32.RegistryValueKind]::ExpandString) ''c:\vigiliatest\BIN\''',
            'Write-Output ("dup=" + ($null -eq $d))'
        ) -join "`n"
        [IO.File]::WriteAllText($ps51Script, $ps51Body, [Text.UTF8Encoding]::new($false))
        $out = & $ps51Path.Source -NoProfile -ExecutionPolicy Bypass -File $ps51Script 2>&1 | Out-String
        Assert-That ($out -match 'add=%VIGILIA_TEST_BIN%;C:\\Tools;C:\\New\\Vigilia\|ExpandString' -and $out -match 'dup=True') 'powershell.exe 5.1 でも同じ結果になる'
    }

    Write-Host 'install.ps1: latest.json 解決（署名の検査まで進んで止まる）'
    $r = Invoke-InstallPs1 @()
    Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'Checksum OK' -and $r.Output -match 'Untrusted signature') 'latest -> 取得・SHA-256 照合を通り、署名の検査で止まる'
    Assert-That (($r.Requests -contains '/latest.json') -and ($r.Requests -contains "/9.9.9/$winName") -and ($r.Requests -contains "/9.9.9/$winName.sha256")) 'latest.json と版付きの zip / .sha256 を取得する'

    Write-Host 'install.ps1: -Version 指定'
    $r = Invoke-InstallPs1 @('-Version', 'v9.9.9')
    Assert-That ($r.Output -match 'Checksum OK' -and $r.Output -match 'Untrusted signature') '-Version v9.9.9 でも同じ経路（先頭の v は無視）'
    Assert-That ($r.Requests -notcontains '/latest.json') '-Version 指定では latest.json を読まない'
    $r = Invoke-InstallPs1 @('-Version', '1.0.0')
    Assert-That ($r.ExitCode -ne 0 -and $r.Requests -contains '/1.0.0/vigilia-1.0.0-x86_64-pc-windows-msvc.zip') '存在しない版は 404 で失敗する'
    $r = Invoke-InstallPs1 @('-Version', '1.0')
    Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'Invalid version') '版の形式が違えば失敗する'

    Write-Host 'install.ps1: SHA-256 不一致'
    $routes["/9.9.9/$winName.sha256"] = $badSumBytes
    $r = Invoke-InstallPs1 @()
    Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'SHA-256 mismatch') '.sha256 と zip が違えば失敗する'
    Assert-That ($r.Output -notmatch 'Checksum OK') '不一致のときは「Checksum OK」を出さない'
    $routes["/9.9.9/$winName.sha256"] = $goodSum

    Write-Host 'install.ps1: 想定外の latest.json'
    $feed = [Text.Encoding]::UTF8.GetString((New-Feed '9.9.9' $dist)).Replace("9.9.9/$winName`"", '../evil.zip"')
    $routes['/latest.json'] = [Text.Encoding]::UTF8.GetBytes($feed)
    $r = Invoke-InstallPs1 @()
    Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'unexpected files') 'latest.json が想定外のパスを指せば失敗し、取りに行かない'
    Assert-That (-not ($r.Requests | Where-Object { $_ -like '*evil*' })) '想定外のパスへは接続しない'
    $routes['/latest.json'] = New-Feed '9.9.9' $dist

    Write-Host 'install.ps1: HTTPS 限定'
    $out = & pwsh -NoProfile -File $installPs1 -BaseUrl 'http://example.com' -InstallDir (Join-Path $WorkDirectory 'install-http') -NoPath 2>&1 | Out-String
    Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'BaseUrl must be https') '非ループバックの http は拒否する'
    # ダウンロードの前に止まるため、レジストリへは何も書かれない（導入先の絶対化で検出される）。
    foreach ($exe in 'pwsh', 'powershell.exe') {
        if (-not (Get-Command $exe -ErrorAction SilentlyContinue)) { continue }
        $requests.Clear()
        $out = & $exe -NoProfile -ExecutionPolicy Bypass -File $installPs1 -BaseUrl $prefix -InstallDir 'HKCU:\Software\VigiliaInstallDirTest' -NoPath 2>&1 | Out-String
        Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'InstallDir must be a file system path' -and $requests.Count -eq 0) "ファイルシステム以外の InstallDir は取得前に拒否する（$exe）"
    }

    Write-Host 'install.ps1: Windows PowerShell 5.1 と配布時の呼び出し方'
    # install.ps1 は保存して 5.1 で実行されるため ASCII だけにしてある（BOM なし UTF-8 の日本語は CP932 として読まれ、改行を巻き込む）。
    $nonAscii = @([IO.File]::ReadAllBytes($installPs1) | Where-Object { $_ -gt 127 }).Count
    Assert-That ($nonAscii -eq 0) 'install.ps1 は ASCII だけで書かれている'
    $ps51 = Get-Command powershell.exe -ErrorAction SilentlyContinue
    if ($ps51) {
        $requests.Clear()
        $target = Join-Path $WorkDirectory 'install-ps51'
        $out = & $ps51.Source -NoProfile -ExecutionPolicy Bypass -File $installPs1 -BaseUrl $prefix -InstallDir $target -NoPath 2>&1 | Out-String
        Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'Checksum OK' -and $out -match 'Untrusted signature') 'powershell.exe 5.1 -File でも取得・照合を通り、署名の検査で止まる'
    } else {
        Write-Host '  skip powershell.exe が無いため 5.1 の検査は省略'
    }
    # `irm | iex` では引数を渡せないため、READMEで案内している script block 形式で -Version を渡せること、
    # 実行後に呼び出し元のスコープへ変数が残らないことを確かめる。
    $target = Join-Path $WorkDirectory 'install-scriptblock'
    $cmd = "`$c = Get-Content -Raw -LiteralPath '$installPs1'; try { & ([scriptblock]::Create(`$c)) -Version 9.9.9 -BaseUrl '$prefix' -InstallDir '$target' -NoPath } catch { Write-Output `$_.Exception.Message }; Write-Output ('leak=' + [bool](Get-Variable ExpectedSigner -ErrorAction SilentlyContinue))"
    $out = & pwsh -NoProfile -Command $cmd 2>&1 | Out-String
    Assert-That ($out -match 'Checksum OK' -and $out -match 'Untrusted signature' -and $out -match 'leak=False') 'script block 形式で -Version を渡せ、変数をスコープに残さない'
    # `irm | iex` 相当: 引数を渡せず、スクリプト文字列を Invoke-Expression で実行する。既定値だけで動くこと
    # （BaseUrl は文字列置換でモックへ向け、LOCALAPPDATA は作業フォルダーへ向けて、実際の導入先には触れない）。
    # 安全のため、置換後のスクリプトが本番 URL を含まずモックの URL を含むことを実行前に検査する。置換が空振りすると
    # 本物の署名済み zip を取得して実際の PATH を書き換え得るので、満たさなければ失敗させて実行しない。
    # （iex 経路では -NoPath を渡せない。実行しても署名の検査で止まる前提なので、この検査が唯一の防壁になる。）
    $prodUrl = 'https://vigilia.kagayoi.com'
    $iexText = ([IO.File]::ReadAllText($installPs1)).Replace($prodUrl, $prefix.TrimEnd('/'))
    $iexSafe = (-not $iexText.Contains($prodUrl)) -and $iexText.Contains($prefix.TrimEnd('/'))
    Assert-That $iexSafe 'iex 用に置換したスクリプトは本番 URL を含まず、モックの URL を含む（満たさなければ iex の検査は実行しない）'
    $iexFile = Join-Path $WorkDirectory 'install-iex.ps1.txt'
    if ($iexSafe) { [IO.File]::WriteAllText($iexFile, $iexText, [Text.UTF8Encoding]::new($false)) }
    foreach ($shell in @(@{ Name = 'pwsh'; Exe = 'pwsh' }, @{ Name = 'powershell.exe 5.1'; Exe = 'powershell.exe' })) {
        if (-not $iexSafe) { break }
        if (-not (Get-Command $shell.Exe -ErrorAction SilentlyContinue)) { Write-Host "  skip $($shell.Name) が無いため iex の検査は省略"; continue }
        $iexLocal = Join-Path $WorkDirectory ('iex-localappdata-' + $shell.Exe)
        New-Item -ItemType Directory -Path $iexLocal -Force | Out-Null
        # 子プロセス側でも実行直前に本番 URL が残っていないことを確かめる（残っていれば実行しない）。
        $cmd = "`$env:LOCALAPPDATA = '$iexLocal'; `$c = Get-Content -Raw -LiteralPath '$iexFile'; if (`$c.Contains('$prodUrl')) { Write-Output 'ABORT: production URL present'; exit 3 }; try { Invoke-Expression `$c } catch { Write-Output `$_.Exception.Message }; Write-Output ('leak=' + [bool](Get-Variable ExpectedSigner -ErrorAction SilentlyContinue))"
        $out = & $shell.Exe -NoProfile -ExecutionPolicy Bypass -Command $cmd 2>&1 | Out-String
        Assert-That ($out -match 'Checksum OK' -and $out -match 'Untrusted signature' -and $out -match 'leak=False') "irm | iex 相当（引数なし・既定値）でも取得・照合を通り、署名の検査で止まる（$($shell.Name)）"
        Assert-That (-not (Test-Path -LiteralPath (Join-Path $iexLocal 'Programs\Vigilia'))) "署名の検査で止まるときは導入先を作らない（$($shell.Name)）"
    }
    }

    # --- install.sh（bash と tar があり、bash が WSL でない環境だけ） --------------------------------
    $bash = Get-Command bash -ErrorAction SilentlyContinue
    $bashOk = $false
    if ($bash -and (Get-Command tar -ErrorAction SilentlyContinue)) {
        $uname = (& $bash.Source -c 'uname -s' 2>$null | Out-String).Trim()
        $bashOk = $uname -match '^(Linux|Darwin|MINGW|MSYS)' -and (-not $IsWindows -or $uname -match '^(MINGW|MSYS)')
    }
    if ($bashOk) {
        # install.sh の契約は POSIX sh（案内は `| sh`）。bashism の混入を見つけるため、非 Windows では sh で実行する（Windows は Git Bash）。
        $shName = if ($IsWindows) { 'bash' } else { 'sh' }
        $shExe = if ($IsWindows) { $bash.Source } else { (Get-Command sh -ErrorAction Stop).Source }
        function Invoke-InstallSh([string[]]$ExtraArgs) {
            $requests.Clear()
            $target = Join-Path $WorkDirectory ('install-sh-' + [guid]::NewGuid().ToString('N'))
            $out = & $shExe $installSh --base-url $prefix --install-dir $target --target x86_64-unknown-linux-gnu @ExtraArgs 2>&1 | Out-String
            return [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = $out; Requests = @($requests.ToArray()); Dir = $target }
        }
        Write-Host 'install.sh: 正常系'
        $r = Invoke-InstallSh @()
        Assert-That ($r.ExitCode -eq 0 -and $r.Output -match 'Checksum OK' -and $r.Output -match 'vigilia 9\.9\.9') 'latest -> 取得・SHA-256 照合・配置・実行まで通る'
        Assert-That (Test-Path -LiteralPath (Join-Path $r.Dir 'vigilia')) 'vigilia が配置される'
        Assert-That (($r.Requests -contains '/latest.json') -and ($r.Requests -contains "/9.9.9/$linuxName") -and ($r.Requests -contains "/9.9.9/$linuxName.sha256")) 'latest.json と版付きの tar.gz / .sha256 を取得する'
        $r = Invoke-InstallSh @('--version', 'v9.9.9')
        Assert-That ($r.ExitCode -eq 0 -and $r.Requests -notcontains '/latest.json') '--version 指定では latest.json を読まない'
        $r = Invoke-InstallSh @('--version', '1.0')
        Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'invalid version') '版の形式が違えば失敗する'
        $r = Invoke-InstallSh @('--version', '1.0.0')
        Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'HTTP 404') '存在しない版は 404 で失敗する'

        Write-Host 'install.sh: SHA-256 不一致'
        $routes["/9.9.9/$linuxName.sha256"] = [Text.Encoding]::ASCII.GetBytes(('0' * 64) + "  $linuxName")
        $r = Invoke-InstallSh @()
        Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'SHA-256 mismatch' -and $r.Output -notmatch 'Checksum OK') '.sha256 と本体が違えば失敗する'
        Assert-That (-not (Test-Path -LiteralPath (Join-Path $r.Dir 'vigilia'))) '失敗時は何も配置しない'
        $routes["/9.9.9/$linuxName.sha256"] = [IO.File]::ReadAllBytes((Join-Path $dist "$linuxName.sha256"))

        Write-Host 'install.sh: フィードのハッシュ不一致・想定外のパス'
        $feed = [Text.Encoding]::UTF8.GetString((New-Feed '9.9.9' $dist))
        $realHash = (Get-FileHash -LiteralPath (Join-Path $dist $linuxName) -Algorithm SHA256).Hash.ToLowerInvariant()
        $routes['/latest.json'] = [Text.Encoding]::UTF8.GetBytes($feed.Replace($realHash, ('1' * 64)))
        $r = Invoke-InstallSh @()
        Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'against latest\.json') 'latest.json のハッシュが実物と違えば失敗する'
        $routes['/latest.json'] = [Text.Encoding]::UTF8.GetBytes($feed.Replace("9.9.9/$linuxName`"", '../evil.tar.gz"'))
        $r = Invoke-InstallSh @()
        Assert-That ($r.ExitCode -ne 0 -and $r.Output -match 'unexpected files' -and -not ($r.Requests | Where-Object { $_ -like '*evil*' })) '想定外のパスを指せば失敗し、取りに行かない'
        $routes['/latest.json'] = New-Feed '9.9.9' $dist

        Write-Host 'install.sh: 制限'
        $out = & $shExe $installSh --base-url 'http://user:pw@127.0.0.1:1' --install-dir (Join-Path $WorkDirectory 'sh-userinfo') --target x86_64-unknown-linux-gnu 2>&1 | Out-String
        Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'user information') 'userinfo 付きの URL は拒否する'
        $out = & $shExe $installSh --help 2>&1 | Out-String
        Assert-That ($LASTEXITCODE -eq 0 -and $out -match '--target TRIPLE' -and $out -match '-h, --help') '--help が最後まで表示される'
        $shPath = $installSh.Replace([string][char]92, '/')
        $out = & $bash.Source -c "unset HOME; VIGILIA_INSTALL_DIR= $shName '$shPath' --base-url '$prefix' --target x86_64-unknown-linux-gnu" 2>&1 | Out-String
        Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'install directory') 'HOME も VIGILIA_INSTALL_DIR も空なら止まる'
        $out = & $shExe $installSh --base-url 'http://example.com' --install-dir (Join-Path $WorkDirectory 'sh-http') --target x86_64-unknown-linux-gnu 2>&1 | Out-String
        Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'must be https') '非ループバックの http は拒否する'
        $out = & $shExe $installSh --base-url $prefix --install-dir (Join-Path $WorkDirectory 'sh-target') --target x86_64-apple-darwin 2>&1 | Out-String
        Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'unsupported target') 'Intel Mac のターゲットは拒否する'
    } else {
        Write-Host 'install.sh: bash / tar が使えないため省略'
    }

    Write-Host 'Publish-R2.ps1 -ValidateOnly'
    $out = & pwsh -NoProfile -File $publish -Version 9.9.9 -DistributionDirectory $dist -ValidateOnly 2>&1 | Out-String
    Assert-That ($LASTEXITCODE -eq 0 -and $out -match '"x86_64-unknown-linux-gnu"' -and $out -match '"schema":1' -and $out -match 'no network access') '正常な入力で latest.json を組み立てる（接続しない）'
    $out = & pwsh -NoProfile -File $publish -Version 9.9.8 -DistributionDirectory $dist -ValidateOnly 2>&1 | Out-String
    Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'unexpected distribution contents') '版と名前が違えば失敗する'
    $badDist = Join-Path $WorkDirectory 'bad-dist'
    Copy-Item -LiteralPath $dist -Destination $badDist -Recurse
    [IO.File]::WriteAllBytes((Join-Path $badDist "$winName.sha256"), $badSumBytes)
    $out = & pwsh -NoProfile -File $publish -Version 9.9.9 -DistributionDirectory $badDist -ValidateOnly 2>&1 | Out-String
    Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'does not match the archive') '.sha256 が本体と違えば失敗する'
    Set-Content -LiteralPath (Join-Path $badDist 'extra.txt') -Value 'x'
    $out = & pwsh -NoProfile -File $publish -Version 9.9.9 -DistributionDirectory $badDist -ValidateOnly 2>&1 | Out-String
    Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'unexpected distribution contents') '余計なファイルがあれば失敗する'
    $out = & pwsh -NoProfile -File $publish -Version 9.9.9 -DistributionDirectory $dist -BaseUrl 'http://x.example' -ValidateOnly 2>&1 | Out-String
    Assert-That ($LASTEXITCODE -ne 0 -and $out -match 'must be https') 'Publish の BaseUrl も https 限定'
} finally {
    $listener.Stop()
    $listener.Close()
    Stop-Job $serverJob -ErrorAction SilentlyContinue
    Remove-Job $serverJob -Force -ErrorAction SilentlyContinue
    # この実行で作った作業フォルダーを片付ける（-KeepWorkDir で残せる）。
    if ($createdWorkDirectory -and -not $KeepWorkDir) { Remove-Item -LiteralPath $WorkDirectory -Recurse -Force -ErrorAction SilentlyContinue }
}

if ($failures.Count -gt 0) { throw "$($failures.Count) check(s) failed: $($failures -join '; ')" }
Write-Host 'All install / publish checks passed.' -ForegroundColor Green
# 失敗系の検査で呼んだ子プロセスの非0終了コードが $LASTEXITCODE に残り、pwsh -File / CI の step を失敗扱いにするため、成功時は明示的に 0 で終える（失敗は上で throw 済み）。
exit 0
