# Install

Mellow runs on macOS (Apple silicon and Intel) and Linux (x86_64 and ARM64).
Windows is not supported; inside WSL, use the Linux instructions.

## Before you install

- Use an interactive terminal on macOS or Linux, on Apple silicon/ARM64 or
  x86_64. You do not need a graphical desktop, custom font or TrueColor support.
- Prebuilt binaries do not require Rust. The installer needs `sh`, `tar`,
  `curl` (or `wget` when running a downloaded script), and `shasum` or `sha256sum`.
- Building with Cargo requires Rust 1.90+ and a working C compiler/linker.
- Git features require system `git`. Language servers and formatters are
  separate optional installs; basic editing needs neither. AI needs explicit
  setup and is not required.

Choose **one** installation method below. Check your platform with `uname -sm`
if you are selecting a release archive manually.

## Install script (macOS and Linux)

```bash
curl -fsSL https://github.com/ritru-labs/mellow/releases/latest/download/install.sh | sh
```

The script picks the right build for your system, checks its SHA-256 checksum
and installs `mellow` to `~/.local/bin`. If that folder is not on your `PATH`
yet, it prints the line to add. The script replaces an existing binary at that location; it does not configure your shell PATH.

Options, set before `sh`:

| Variable | Effect |
| --- | --- |
| `MELLOW_VERSION=0.2.1` | Install that version instead of the latest |
| `MELLOW_INSTALL_DIR=/some/dir` | Install somewhere other than `~/.local/bin` |

For example, choose a different install directory:

```bash
curl -fsSL https://github.com/ritru-labs/mellow/releases/latest/download/install.sh | MELLOW_INSTALL_DIR="$HOME/bin" sh
```

## Homebrew (macOS and Linux)

```bash
brew install ritru-labs/tap/mellow
```

On macOS, use Homebrew or the install script rather than downloading the archive
in a browser: the builds are not yet signed by Apple, so macOS may block
browser-downloaded copies.

## Debian and Ubuntu

```bash
curl -fsSLO https://github.com/ritru-labs/mellow/releases/download/v0.2.1/mellow_0.2.1_amd64.deb
sudo apt install ./mellow_0.2.1_amd64.deb
```

On ARM64, use `mellow_0.2.1_arm64.deb`.

## Fedora, RHEL, Rocky, Alma

```bash
sudo dnf install https://github.com/ritru-labs/mellow/releases/download/v0.2.1/mellow-0.2.1-1.x86_64.rpm
```

On ARM64, use `mellow-0.2.1-1.aarch64.rpm`. On Amazon Linux 2, use `yum`
instead of `dnf`. On openSUSE:

```bash
sudo rpm -i https://github.com/ritru-labs/mellow/releases/download/v0.2.1/mellow-0.2.1-1.x86_64.rpm
```

The release workflow targets static musl Linux binaries to reduce dependencies
on a distribution-specific C library.

**Repository-reported validation coverage** (install, edit, save and quit in a real terminal): Amazon
Linux 2 and 2023, Rocky Linux 9, AlmaLinux 8, CentOS Stream 9, Fedora,
openSUSE Leap 15.6, Debian 11 and 12, Ubuntu 20.04, 22.04 and 24.04, Alpine
3.20 and Arch Linux, on x86_64 and ARM64; and macOS on Apple silicon.

## Any Linux or macOS, by hand

Download the `.tar.gz` for your system from the
[latest release](https://github.com/ritru-labs/mellow/releases/latest), check it
against `SHA256SUMS`, unpack it and put `mellow` on your `PATH`:

| System | Archive |
| --- | --- |
| macOS, Apple silicon | `mellow-<version>-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `mellow-<version>-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `mellow-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 | `mellow-<version>-aarch64-unknown-linux-musl.tar.gz` |

## With Cargo

If you have Rust 1.90 or newer:

```bash
cargo install mellow --locked
```

## From source

With Rust 1.90 or newer:

```bash
git clone https://github.com/ritru-labs/mellow.git
cd mellow
cargo install --locked --path .
```

## Check it worked

```bash
mellow --version
```

The version should print as `mellow 0.2.1` for the documented release (a newer
installed release will print its own version). Run `mellow --help` for CLI usage,
then follow [Make your first edit](getting-started.md). If the shell cannot find
it, see [PATH troubleshooting](faq.md#mellow-command-not-found).

## Update and uninstall

| Installed with | Update | Uninstall |
| --- | --- | --- |
| Install script | run it again | `rm ~/.local/bin/mellow` |
| Homebrew | `brew upgrade mellow` | `brew uninstall mellow` |
| `.deb` | install the newer `.deb` | `sudo apt remove mellow` |
| `.rpm` | install the newer `.rpm` | `sudo dnf remove mellow` |
| Cargo | `cargo install mellow --locked` | `cargo uninstall mellow` |

Your settings and recovery data live in `~/.config/mellow` and
`~/.local/state/mellow`; delete those folders to remove them too.
