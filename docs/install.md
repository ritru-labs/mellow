# Install

Mellow runs on macOS (Apple silicon and Intel) and Linux (x86_64 and ARM64).
Windows is not supported; inside WSL, use the Linux instructions.

## Install script (macOS and Linux)

```bash
curl -fsSL https://github.com/ritru-labs/mellow/releases/latest/download/install.sh | sh
```

The script picks the right build for your system, checks its SHA-256 checksum
and installs `mellow` to `~/.local/bin`. If that folder is not on your `PATH`
yet, it prints the line to add. Nothing else on your system is changed.

Options, set before `sh`:

| Variable | Effect |
| --- | --- |
| `MELLOW_VERSION=0.2.0` | Install that version instead of the latest |
| `MELLOW_INSTALL_DIR=/some/dir` | Install somewhere other than `~/.local/bin` |

For example: `curl -fsSL …/install.sh | MELLOW_INSTALL_DIR="$HOME/bin" sh`.

## Homebrew (macOS and Linux)

```bash
brew install ritru-labs/tap/mellow
```

On macOS, use Homebrew or the install script rather than downloading the archive
in a browser: the builds are not yet signed by Apple, so macOS blocks
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

The Linux builds are static, so they run on old and new distributions alike.

**Tested on** (install, then edit, save and quit in a real terminal): Amazon
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
