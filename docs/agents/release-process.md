# リリースと配信

[AGENTS.md](../../AGENTS.md) の詳細。

配布は Cloudflare R2（バケット `vigilia-updates`、公開 URL https://vigilia.kagayoi.com）だけで、GitHub Release は使わない。

- 手順: main のコミットに軽量タグ `v<版>` を付けて push する（タグと `Cargo.toml` の版は一致させる。版を上げるのは `/vava`）。`.github/workflows/release.yml` が guard → build（4 ターゲット）→ sign-windows → publish の順で動き、R2 へ配信する。同じタグでの再実行だけ workflow_dispatch を使う（`gh workflow run release.yml --ref v<版>`）（`skip_upload` で配信なしの通し確認もできる）。
- 同時実行: `release.yml` の concurrency は repo 全体で `group: release`（`cancel-in-progress: false`）。別のタグが並走しても `latest.json` を取り合って後退させない。
- ターゲット: Windows x64（`x86_64-pc-windows-msvc`）、macOS Apple Silicon（`aarch64-apple-darwin`）、Linux x64 / arm64（`x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu`）。macOS Intel・Windows arm64 は対象外。ターゲット表は `release.yml` の matrix・`scripts/Publish-R2.ps1`・`install.ps1` / `install.sh`・`tests/Test-Install.ps1` で揃える。
- `install.ps1` は例外として、コメントも文字列も ASCII（英語）だけで書く。保存して Windows PowerShell 5.1 で実行されると、BOM なし UTF-8 を ANSI(CP932) として読み、日本語が改行を巻き込んで構文が壊れるため（`tests/Test-Install.ps1` が ASCII であることと 5.1 での実行を確かめる）。
- Linux の glibc: build ジョブが `objdump -T` で要求する glibc のシンボル版をログに残すだけで、下限は保証しない（配布物は ubuntu-24.04 でビルド）。`install.sh` は配置前に展開した `vigilia --version` を実行し、動かなければ置き換えない。
- テスト: `ci.yml` が全 OS で `tests/Test-Install.ps1`（Windows では `install.ps1` と Windows PowerShell 5.1 も）を実行し、`release.yml` の Linux x64 build が `cargo test --locked` を通す。
- 署名: Windows の exe だけ、CI 内で Sigilune（Certum のクラウド鍵）が署名し、署名者 CN を検証する。macOS は公証なしで、リンカーの ad-hoc 署名を `codesign --verify` で確かめるまで。
- R2 の置き場: `<版>/vigilia-<版>-<ターゲット>.zip`（Windows）・`.tar.gz`（macOS / Linux）と `.sha256`（版付き・不変・`public, max-age=31536000, immutable`）、最後に `latest.json`（固定名・`no-cache, must-revalidate`、形式は `scripts/Publish-R2.ps1` の冒頭を参照）。版付きは新規作成のみで、同内容なら成功・別内容なら失敗する。公開 URL から取り直して照合してから `latest.json` を置く。公開中の `latest.json` がより新しい版なら更新しない。
- GitHub の設定: Environment `release`（許可 ref は版タグ `v[0-9]*.[0-9]*.[0-9]*`（type: tag）だけ、承認者なし）に Secrets `SIGILUNE_EMAIL` / `SIGILUNE_TOTP_URI`（署名）と `CLOUDFLARE_API_TOKEN`（配信。R2 の書込みとキャッシュ purge 権限）を置く。Secrets は sign-windows の署名 step と publish の配信 step にだけ渡し、build・梱包・検証 step には渡さない。Cloudflare のアカウント ID は秘密ではなく `Publish-R2.ps1` の既定値に持つ。
- Cloudflare 側: R2 バケット `vigilia-updates` を作り、カスタムドメイン `vigilia.kagayoi.com`（zone `kagayoi.com`）を割り当てる。
- `install.ps1` / `install.sh` / `scripts/Publish-R2.ps1` / `scripts/R2*` を変えたら `pwsh -NoProfile -File tests/Test-Install.ps1` を通す（R2 にも Certum にも接続しない）。`R2Transfer.Core.cs` と `R2Distribution.Common.ps1` は Sigilune・Edictora・Skilvella と共通の同梱コピーなので、片方だけ直さない。
- `release.yml` の配信 step で `$LASTEXITCODE` を見ない。`Publish-R2.ps1` は失敗を例外で知らせ、成功後もスクリプト内部の終了コードが残るため、見ると誤って失敗扱いになる。
- 配布物のビルドはキャッシュを使わない（`ci.yml` と違い rust-cache を付けない）。再ビルドは同じ版でもバイト列が変わるため、配信済みの版を別のビルドで上書きしようとすると失敗する。配信だけやり直すときは publish ジョブだけを再実行する。
