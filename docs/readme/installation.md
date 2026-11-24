# Installation

Back to [README](../../README.md).

Prebuilt binaries are published at https://vigilia.kagayoi.com (Cloudflare R2). Each download is checked against its SHA-256 checksum; the Windows executable is Authenticode-signed and its signer is verified too.

Windows x64 (PowerShell):

```powershell
irm https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.ps1 | iex
```

macOS (Apple Silicon) and Linux (x86_64 / aarch64; glibc-based distributions, built on ubuntu-24.04):

```sh
curl -fsSL https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.sh | sh
```

Both scripts install the latest release. To pin a version with the shell script, use `sh -s -- --version 1.0.1`. Arguments cannot be passed through `| iex`, so on PowerShell use a script block instead:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/1llum1n4t1s/Vigilia/main/install.ps1))) -Version 1.0.1
```

`install.sh` puts `vigilia` in `~/.local/bin` (`--install-dir` or `VIGILIA_INSTALL_DIR` to change it) and prints a PATH hint if needed.

You can also download an archive directly. `latest.json` lists the newest version and, per target, the archive, its checksum file and its SHA-256:

| Target | Archive |
| --- | --- |
| Windows x64 | `<version>/vigilia-<version>-x86_64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `<version>/vigilia-<version>-aarch64-apple-darwin.tar.gz` |
| Linux x86_64 | `<version>/vigilia-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `<version>/vigilia-<version>-aarch64-unknown-linux-gnu.tar.gz` |

Each archive has a `.sha256` file next to it (`sha256sum` format). The macOS binary is ad-hoc signed and not notarized.

Alternatively, build from source with Rust (a stable toolchain that supports edition 2024):

```sh
cargo install --git https://github.com/1llum1n4t1s/Vigilia
```

or clone the repository and build it:

```sh
cargo build --release   # target/release/vigilia (vigilia.exe on Windows)
```

CI builds, tests and lints on Windows x64, Apple Silicon macOS and Linux. Intel Macs are not supported.

Authentication borrows your existing GitHub CLI login (`gh auth token`), so `gh` must be installed and signed in. If `GH_TOKEN` or `GITHUB_TOKEN` is set, it takes precedence. The token is never written to logs, output or the cache.
