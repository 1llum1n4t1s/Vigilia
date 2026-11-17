#requires -Version 7.2
<#
.SYNOPSIS
Vigilia の配布アーカイブ（4 ターゲット）を Cloudflare R2（vigilia-updates / https://vigilia.kagayoi.com）へ配信する。

.DESCRIPTION
release.yml（release/* ブランチの push）の publish ジョブから呼ぶ。GitHub Release もタグも使わず、R2 を唯一の配布元にする。
お手本は Sigilune の scripts/Publish-R2.ps1。違いは、ターゲットが複数（OS・CPU 別）で latest.json が各ターゲットを表す点だけ。

置き場（バケット直下）:
  <ver>/vigilia-<ver>-<target>.zip|.tar.gz          版付き・不変（immutable）。<target> は下の $Targets
  <ver>/vigilia-<ver>-<target>.zip|.tar.gz.sha256   版付き・不変（immutable）。sha256sum 互換 "<hash>  <file>"
  latest.json                                       固定名のフィード。最後に置き、固定名だけキャッシュを purge する

latest.json の形式（install.ps1 / install.sh が読む。パスは BaseUrl からの相対）:
  {"schema":1,"version":"1.0.0","targets":{"<target>":{"archive":"<ver>/…","checksum":"<ver>/….sha256","sha256":"<64桁>"}, …}}

配信の順序と安全策:
  1. 入力検査（8 ファイルだけか・.sha256 の中身が実ハッシュと一致するか）と、bucket・zone の権限確認を、何も書く前に行う。
  2. 版付きファイルを先に置く。同じキーが既にあって内容が同一なら成功（再実行を許す）。
     内容が違えば上書きせず失敗する（再ビルドで同じ版の別内容を上げない）。
  3. 公開 URL（Cloudflare 経由）から版付きファイルを取り直して照合する。公開ドメイン未設定・誤設定のまま
     フィードだけを進めないための確認。
  4. R2 の latest.json の版が今回より新しければ latest.json は更新しない（後退防止）。同じか古ければ更新する。
  5. 固定名（latest.json）だけを purge する。purge の失敗は警告（latest.json は no-cache で配るため）。

認証: 環境変数 CLOUDFLARE_API_TOKEN（R2 書込み＋キャッシュ purge 権限。CI では Environment の Secret）。
転送実装は Sigilune・Edictora・Skilvella と同一の R2Transfer.Core.cs / R2Distribution.Common.ps1（同梱コピー。片方だけ直さない）。
#>
[CmdletBinding()]
param(
    # 配信する版（例 1.0.0）。Cargo.toml の版と一致していること（呼び出し側の guard で保証する）。
    [Parameter(Mandatory)][string]$Version,
    # 4 ターゲットのアーカイブと、それぞれの .sha256 だけを置いたフォルダー。
    [Parameter(Mandatory)][string]$DistributionDirectory,
    [string]$Bucket = 'vigilia-updates',
    [string]$BaseUrl = 'https://vigilia.kagayoi.com',
    # Cloudflare アカウント ID（秘密ではない。他アプリと同じアカウント）。
    [string]$AccountId = '10901bfadbf1005164774a7350082985',
    # 入力検査と latest.json の生成だけを行い、ネットワークへは一切接続しない（ローカル検証用）。
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "version must look like 1.2.3: $Version" }
$BaseUrl = $BaseUrl.TrimEnd('/')
if ($BaseUrl -notmatch '^https://') { throw "BaseUrl must be https: $BaseUrl" }
if (-not [IO.Path]::IsPathFullyQualified($DistributionDirectory)) { throw 'Specify an absolute path to the distribution directory.' }

# 配布するターゲットと拡張子。install.ps1 / install.sh / release.yml と同じ表（変えるときは release.yml・Publish-R2.ps1・install.ps1・install.sh・tests/Test-Install.ps1 の 5 か所を揃える）。
$Targets = [ordered]@{
    'x86_64-pc-windows-msvc'    = 'zip'
    'aarch64-apple-darwin'      = 'tar.gz'
    'x86_64-unknown-linux-gnu'  = 'tar.gz'
    'aarch64-unknown-linux-gnu' = 'tar.gz'
}
$contentTypes = @{ 'zip' = 'application/zip'; 'tar.gz' = 'application/gzip' }

# --- 入力検査 ------------------------------------------------------------------------------------
$directory = Get-Item -LiteralPath $DistributionDirectory -ErrorAction Stop
if (-not $directory.PSIsContainer) { throw 'The distribution path must be a directory.' }

$items = [System.Collections.Generic.List[object]]::new()
foreach ($target in $Targets.Keys) {
    $name = "vigilia-$Version-$target.$($Targets[$target])"
    $items.Add([pscustomobject]@{ Target = $target; Name = $name; Key = "$Version/$name"; Type = $contentTypes[$Targets[$target]]; Sum = $false })
    $items.Add([pscustomobject]@{ Target = $target; Name = "$name.sha256"; Key = "$Version/$name.sha256"; Type = 'text/plain; charset=utf-8'; Sum = $true })
}
# 余計なファイル・足りないファイルを許さない（別の版や別ターゲットの混入を防ぐ）。
$expectedNames = @($items | ForEach-Object Name | Sort-Object)
$actualNames = @(Get-ChildItem -LiteralPath $directory.FullName -File -Force | ForEach-Object Name | Sort-Object)
if (Compare-Object $expectedNames $actualNames) { throw "unexpected distribution contents: $($actualNames -join ', ')" }

$feedTargets = [ordered]@{}
$hashes = @{}
foreach ($target in $Targets.Keys) {
    $name = "vigilia-$Version-$target.$($Targets[$target])"
    $archive = Join-Path $directory.FullName $name
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    $sumText = (Get-Content -LiteralPath "$archive.sha256" -Raw).Trim()
    # 形式は `<hash>  <アーカイブ名>`（sha256sum 互換）。install.* は先頭の語だけを使う。
    if ($sumText -cne "$hash  $name") { throw "$name.sha256 does not match the archive (expected '$hash  $name')" }
    $hashes[$name] = $hash
    $feedTargets[$target] = [ordered]@{ archive = "$Version/$name"; checksum = "$Version/$name.sha256"; sha256 = $hash }
}
$feedJson = ([ordered]@{ schema = 1; version = $Version; targets = $feedTargets } | ConvertTo-Json -Depth 5 -Compress) + "`n"

$work = Join-Path ([IO.Path]::GetTempPath()) ('vigilia-publish-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$feedFile = Join-Path $work 'latest.json'
[IO.File]::WriteAllText($feedFile, $feedJson, [Text.UTF8Encoding]::new($false))
Write-Host "package ok: version $Version ($($Targets.Count) targets)"
foreach ($name in $hashes.Keys | Sort-Object) { Write-Host ("  {0}  sha256={1}" -f $name, $hashes[$name]) }
Write-Host "feed: $($feedJson.Trim())"
if ($ValidateOnly) {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'ValidateOnly: no network access.'
    $global:LASTEXITCODE = 0
    return
}

try {
    if (-not $env:CLOUDFLARE_API_TOKEN) { throw 'CLOUDFLARE_API_TOKEN is not set' }
    . (Join-Path $PSScriptRoot 'R2Distribution.Common.ps1')
    $api = 'https://api.cloudflare.com/client/v4'
    $headers = @{ Authorization = "Bearer $($env:CLOUDFLARE_API_TOKEN)" }

    # --- 書く前の権限確認（途中まで公開された状態を防ぐ） -----------------------------------------
    $bucketResponse = Invoke-RestMethod -Uri "$api/accounts/$AccountId/r2/buckets/$([uri]::EscapeDataString($Bucket))" `
        -Headers $headers -TimeoutSec 30 -MaximumRedirection 0
    if (-not $bucketResponse.success -or $bucketResponse.result.name -cne $Bucket) { throw "R2 bucket '$Bucket' was not found" }
    # <sub>.kagayoi.com -> kagayoi.com（apex）。キャッシュ purge に zone ID が要る。
    $zoneName = ([uri]$BaseUrl).Host -replace '^[^.]+\.', ''
    $zoneResponse = Invoke-RestMethod -Uri "$api/zones?name=$zoneName" -Headers $headers -TimeoutSec 30 -MaximumRedirection 0
    if (-not $zoneResponse.success -or @($zoneResponse.result).Count -eq 0) { throw "Cloudflare zone '$zoneName' was not found" }
    $zoneId = $zoneResponse.result[0].id

    # --- 後退防止の判定材料: いま公開されている latest.json の版 ----------------------------------
    # 固定名は no-cache で配っているため、公開 URL から読む。無い（404）なら初回配信。
    $remote = Invoke-WebRequest -Uri "$BaseUrl/latest.json" -SkipHttpErrorCheck -MaximumRedirection 0 -TimeoutSec 30
    $remoteVersion = $null
    if ($remote.StatusCode -eq 200) {
        $remoteFeed = $remote.Content | ConvertFrom-Json
        # 壊れたフィードを黙って上書きしない（原因を確かめてから）。
        if (-not ($remoteFeed.PSObject.Properties.Name -contains 'version') -or "$($remoteFeed.version)" -notmatch '^\d+\.\d+\.\d+$') {
            throw "the published latest.json has no valid version; check $BaseUrl/latest.json"
        }
        $remoteVersion = [version]$remoteFeed.version
    } elseif ($remote.StatusCode -ne 404) {
        throw "could not read $BaseUrl/latest.json (HTTP $($remote.StatusCode))"
    }

    # --- 版付き（不変）ファイルを先に置く ----------------------------------------------------------
    # Immutable: 既存と同一なら成功（Changed=false）、既存と内容が違えば照合で失敗し上書きしない。
    $immutableCache = 'public, max-age=31536000, immutable'
    foreach ($item in $items) {
        $result = Publish-R2Object -AccountId $AccountId -Bucket $Bucket -FilePath (Join-Path $directory.FullName $item.Name) -Key $item.Key `
            -ApiToken $env:CLOUDFLARE_API_TOKEN -ContentType $item.Type -CacheControl $immutableCache -Immutable
        Write-Host ("  {0}  {1}" -f $(if ($result.Changed) { 'uploaded ' } else { 'unchanged' }), $item.Key)
    }

    # --- 公開 URL から取り直して照合（フィードを進める前の最終確認） --------------------------------
    foreach ($target in $Targets.Keys) {
        $name = "vigilia-$Version-$target.$($Targets[$target])"
        $key = "$Version/$name"
        $publicSum = (Invoke-WebRequest -Uri "$BaseUrl/$key.sha256" -MaximumRedirection 0 -TimeoutSec 60).Content
        if ($publicSum -is [byte[]]) { $publicSum = [Text.Encoding]::UTF8.GetString($publicSum) }
        if ($publicSum.Trim() -cne "$($hashes[$name])  $name") { throw "$BaseUrl/$key.sha256 does not serve the uploaded checksum" }
        $publicArchive = Join-Path $work $name
        Invoke-WebRequest -Uri "$BaseUrl/$key" -OutFile $publicArchive -MaximumRedirection 0 -TimeoutSec 600
        if ((Get-FileHash -LiteralPath $publicArchive -Algorithm SHA256).Hash.ToLowerInvariant() -cne $hashes[$name]) {
            throw "$BaseUrl/$key does not serve the uploaded archive"
        }
        Remove-Item -LiteralPath $publicArchive -Force
        Write-Host "public URL verified: $BaseUrl/$key"
    }

    # --- フィード（最後） ---------------------------------------------------------------------------
    if ($remoteVersion -and $remoteVersion -gt [version]$Version) {
        # 古い版の再実行などで latest を後退させない。版付きファイルは置いたので配信自体は成功。
        Write-Warning "published latest.json is already $remoteVersion (newer than $Version); latest.json was left unchanged."
        $global:LASTEXITCODE = 0
        return
    }
    # 固定名は毎回再検証させる。Publish-R2Object は既存の ETag を If-Match に使い、同時更新を検出する。
    $null = Publish-R2Object -AccountId $AccountId -Bucket $Bucket -FilePath $feedFile -Key 'latest.json' `
        -ApiToken $env:CLOUDFLARE_API_TOKEN -ContentType 'application/json' -CacheControl 'no-cache, must-revalidate'
    Write-Host '  uploaded   latest.json'

    # --- 固定名だけ purge（版付きは URL が一意なので不要）。失敗は警告にとどめる -------------------
    try {
        $body = @{ files = @("$BaseUrl/latest.json") } | ConvertTo-Json -Compress
        $purge = Invoke-RestMethod -Method Post -Uri "$api/zones/$zoneId/purge_cache" -Headers $headers `
            -ContentType 'application/json' -Body $body -TimeoutSec 30 -MaximumRedirection 0
        if (-not $purge.success) { throw ($purge.errors | ConvertTo-Json -Compress) }
        Write-Host '  purged     latest.json'
    } catch {
        Write-Warning "uploaded, but the cache purge failed: $($_.Exception.Message)"
    }
    Write-Host "published Vigilia $Version to $BaseUrl"
    # GitHub Actions の pwsh は step 末尾で $LASTEXITCODE を終了コードにするため、内部のネイティブ呼び出しの値を残さない。
    $global:LASTEXITCODE = 0
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
