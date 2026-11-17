#!/bin/sh
# Install Vigilia (vigilia) for macOS (Apple Silicon) or Linux (x86_64 / aarch64, glibc).
#
# Downloads the release archive from https://vigilia.kagayoi.com (Cloudflare R2; GitHub Releases are not used),
# checks its SHA-256 against the published checksum (and the hash in latest.json), then installs it.
#
#   curl -fsSL https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.sh | sh -s -- --version 1.0.1
#
# Options:
#   --version X.Y.Z     Release to install (default: the newest, read from latest.json)
#   --install-dir DIR   Where to put vigilia (default: $VIGILIA_INSTALL_DIR or $HOME/.local/bin)
#   --base-url URL      Download site (default: https://vigilia.kagayoi.com). https only; plain http is
#                       accepted for a loopback address so that tests can use a local mock server.
#   --target TRIPLE     Override the detected target (for testing)
#   -h, --help          Show this help
#
# Layout of the download site (same as install.ps1):
#   latest.json                                       {"schema","version","targets":{<target>:{"archive","checksum","sha256"}}}
#   <ver>/vigilia-<ver>-<target>.tar.gz               immutable
#   <ver>/vigilia-<ver>-<target>.tar.gz.sha256        immutable
#
# 設計メモ:
# - POSIX sh で書く（macOS 標準の bash 3.2・dash・busybox でも動かす）。jq は要求せず、コンパクトな JSON を sed で読む。
# - 版は x.y.z だけを受け付け、取得先のキーは版とターゲットから組み立てる。latest.json の値は照合に使うだけで、
#   任意のパスへ飛ばされない。リダイレクトは辿らない（curl に -L を付けない）。
# - シェルの設定ファイルは書き換えない。PATH に無い場合は案内だけを出す。
set -eu

BASE_URL="https://vigilia.kagayoi.com"
VERSION="latest"
# 既定の配置先。VIGILIA_INSTALL_DIR も HOME も空なら空のままにし、後で止める（"/.local/bin" のような意図しない場所へ置かない）。
INSTALL_DIR="${VIGILIA_INSTALL_DIR:-}"
if [ -z "$INSTALL_DIR" ] && [ -n "${HOME:-}" ]; then INSTALL_DIR="$HOME/.local/bin"; fi
TARGET=""

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
step() { printf '==> %s\n' "$*"; }

# curl | sh で実行されると $0 がスクリプト自身ではないため、ヘルプは本文に直接持つ。
usage() {
  cat <<'USAGE'
Install Vigilia (vigilia) for macOS (Apple Silicon) or Linux (x86_64 / aarch64, glibc).

Usage: install.sh [options]
  --version X.Y.Z     Release to install (default: the newest, read from latest.json)
  --install-dir DIR   Where to put vigilia (default: $VIGILIA_INSTALL_DIR or $HOME/.local/bin)
  --base-url URL      Download site (default: https://vigilia.kagayoi.com). https only; plain http is
                      accepted for a loopback address so that tests can use a local mock server.
  --target TRIPLE     Override the detected target (for testing)
  -h, --help          Show this help
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift 2 ;;
    --install-dir) [ $# -ge 2 ] || die "--install-dir needs a value"; INSTALL_DIR="$2"; shift 2 ;;
    --base-url) [ $# -ge 2 ] || die "--base-url needs a value"; BASE_URL="$2"; shift 2 ;;
    --target) [ $# -ge 2 ] || die "--target needs a value"; TARGET="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[ -n "$INSTALL_DIR" ] || die "could not determine the install directory (HOME is not set); use --install-dir"
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

# SHA-256 の計算コマンド（Linux は sha256sum、macOS は shasum）。
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
  die "sha256sum or shasum is required"
fi

# --- 配布元は https 限定（テスト用にループバックだけ http を許す） ---------------------------------
BASE_URL="${BASE_URL%/}"
# userinfo（user:pass@host）付きの URL は、ホストの見え方を取り違えさせる経路になるため拒否する。
_authority="${BASE_URL#*://}"
_authority="${_authority%%/*}"
case "$_authority" in *@*) die "base URL must not contain user information: $BASE_URL" ;; esac
case "$BASE_URL" in
  https://*) PROTO="=https" ;;
  http://127.0.0.1|http://127.0.0.1:*|http://localhost|http://localhost:*|http://\[::1\]|http://\[::1\]:*) PROTO="=http" ;;
  *) die "base URL must be https: $BASE_URL" ;;
esac

# --- ターゲットの判定 --------------------------------------------------------------------------
if [ -z "$TARGET" ]; then
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin)
      # Rosetta 下のシェルは uname -m が x86_64 になる。実機が Apple Silicon かを別に確かめる。
      if [ "$arch" = "x86_64" ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = "1" ]; then arch="arm64"; fi
      case "$arch" in
        arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
        *) die "Intel Macs are not supported (only Apple Silicon is published)" ;;
      esac ;;
    Linux)
      case "$arch" in
        x86_64|amd64) TARGET="x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
        *) die "unsupported CPU: $arch" ;;
      esac
      # 配布物は glibc 向け。musl（Alpine など）では動かないので、ダウンロード前に止める。
      if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
        die "this build needs glibc; musl-based systems (e.g. Alpine) are not supported"
      fi ;;
    *) die "unsupported OS: $os (on Windows use install.ps1)" ;;
  esac
fi
case "$TARGET" in
  aarch64-apple-darwin|x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
  *) die "unsupported target: $TARGET" ;;
esac

WORK="$(mktemp -d "${TMPDIR:-/tmp}/vigilia-install.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT INT TERM

# 200 以外（リダイレクトを含む）は失敗にする。
fetch() {
  _code="$(curl -sS --proto "$PROTO" --connect-timeout 20 --max-time 600 -A vigilia-installer \
    -o "$2" -w '%{http_code}' "$1")" || die "download failed: $1"
  [ "$_code" = "200" ] || die "download failed (HTTP $_code): $1"
}

# --- 版の決定 ---------------------------------------------------------------------------------
WANT="${VERSION#v}"
FEED=""
if [ "$WANT" = "latest" ]; then
  step "Looking up the latest release"
  fetch "$BASE_URL/latest.json" "$WORK/latest.json"
  # 空白・改行を除いてから読む（コンパクトな JSON を前提にした抽出のため）。
  FEED="$(tr -d ' \t\r\n' < "$WORK/latest.json")"
  WANT="$(printf '%s' "$FEED" | sed -n 's/.*"version":"\([0-9][0-9.]*\)".*/\1/p')"
fi
case "$WANT" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) die "invalid version '$WANT'" ;;
esac
printf '%s' "$WANT" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "invalid version '$WANT'"

NAME="vigilia-$WANT-$TARGET.tar.gz"
KEY="$WANT/$NAME"

FEED_HASH=""
if [ -n "$FEED" ]; then
  # フィードが想定どおりのパスを指していること、ハッシュが載っていることを確かめる。
  printf '%s' "$FEED" | grep -Fq "\"$TARGET\":{\"archive\":\"$KEY\",\"checksum\":\"$KEY.sha256\"," \
    || die "latest.json points to unexpected files for $WANT ($TARGET)"
  FEED_HASH="$(printf '%s' "$FEED" | sed -n "s/.*\"$TARGET\":{[^}]*\"sha256\":\"\\([0-9a-fA-F]\\{64\\}\\)\".*/\\1/p" | tr 'A-F' 'a-f')"
  [ -n "$FEED_HASH" ] || die "latest.json has no checksum for $TARGET"
fi

# --- ダウンロードと SHA-256 照合 ---------------------------------------------------------------
step "Downloading $NAME"
fetch "$BASE_URL/$KEY" "$WORK/$NAME"
fetch "$BASE_URL/$KEY.sha256" "$WORK/$NAME.sha256"
EXPECTED="$(cut -d ' ' -f 1 < "$WORK/$NAME.sha256" | tr 'A-F' 'a-f')"
ACTUAL="$(sha256_of "$WORK/$NAME")"
[ "$EXPECTED" = "$ACTUAL" ] || die "SHA-256 mismatch for $NAME (expected $EXPECTED, got $ACTUAL)"
if [ -n "$FEED_HASH" ] && [ "$FEED_HASH" != "$ACTUAL" ]; then
  die "SHA-256 mismatch for $NAME against latest.json (expected $FEED_HASH, got $ACTUAL)"
fi
step "Checksum OK"

# --- 展開（想定外のファイル名があれば止める） --------------------------------------------------
LISTING="$(tar -tzf "$WORK/$NAME")" || die "could not read the archive"
for entry in $LISTING; do
  case "$entry" in
    vigilia|README.md|README.ja.md|LICENSE) ;;
    *) die "unexpected entry in the archive: $entry" ;;
  esac
done
printf '%s\n' "$LISTING" | grep -qx 'vigilia' || die "the archive does not contain vigilia"
mkdir "$WORK/extract"
tar -xzf "$WORK/$NAME" -C "$WORK/extract"
[ -f "$WORK/extract/vigilia" ] || die "the archive does not contain vigilia"
chmod 755 "$WORK/extract/vigilia"

# macOS はリンカーが付ける ad-hoc 署名（公証なし）が壊れていないことだけ確かめる。
if [ "$TARGET" = "aarch64-apple-darwin" ] && command -v codesign >/dev/null 2>&1; then
  codesign --verify "$WORK/extract/vigilia" 2>/dev/null || die "the code signature of vigilia is invalid"
fi

# 置き換える前に、展開した実行ファイルが実際にこの環境で動き、期待した版を出すことを確かめる
# （glibc が古い・CPU が違うなどで動かない場合に、動いている既存の vigilia を壊さない）。
_reported="$("$WORK/extract/vigilia" --version 2>&1)" || die "the downloaded vigilia does not run on this system: $_reported"
[ "$_reported" = "vigilia $WANT" ] || die "unexpected version from the downloaded binary: $_reported"

# --- 配置（同じ場所へ一時名で置いてから rename し、書き込み途中の状態を見せない） -----------------
mkdir -p "$INSTALL_DIR"
cp "$WORK/extract/vigilia" "$INSTALL_DIR/.vigilia.new.$$"
mv -f "$INSTALL_DIR/.vigilia.new.$$" "$INSTALL_DIR/vigilia"
step "Installed to $INSTALL_DIR/vigilia"

case ":${PATH:-}:" in
  *":$INSTALL_DIR:"*) ;;
  *) printf '\nAdd %s to your PATH, for example:\n  export PATH="%s:$PATH"\n' "$INSTALL_DIR" "$INSTALL_DIR" ;;
esac

"$INSTALL_DIR/vigilia" --version
printf '\nNext: vigilia needs the GitHub CLI signed in (gh auth login) or GH_TOKEN / GITHUB_TOKEN set.\n  vigilia budget\n  vigilia watch HEAD\n'
