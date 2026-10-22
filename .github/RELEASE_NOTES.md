Choose the ZIP for your computer:

| Computer | Download | Open after extracting |
| --- | --- | --- |
| Windows, Intel/AMD 64-bit | **four-words-windows-x64.zip** | **Four Words.exe** |
| Linux, Intel/AMD 64-bit | **four-words-linux-x64.zip** | `./four-words-gui` |
| Mac, Apple Silicon (M1 or newer) | **four-words-macos-arm64.zip** | **Four Words.app** |
| Mac, Intel | **four-words-macos-x64.zip** | **Four Words.app** |

On Windows, right-click the ZIP and choose **Extract All**. On macOS, you can
drag **Four Words.app** to Applications. The Mac app is ad-hoc signed, but not
Developer ID signed or notarized; if blocked, use **System Settings → Privacy &
Security → Open Anyway** after trying to open it.
[Apple's instructions](https://support.apple.com/en-us/102445).

Linux requires Ubuntu 24.04 or a compatible newer distribution with a desktop
and OpenGL. On Ubuntu, install runtime libraries if needed:

```sh
sudo apt install libxkbcommon0 libwayland-client0 libegl1 libgl1 libxcb1
```

Every package includes the desktop app, command-line tool, and license notices.
No Rust, WSL, or internet connection is needed to run it.

Choose your languages and password limits, then click **Copy password**.
Features include English, French, German, Spanish, and Latin word lists; an
entropy display; optional word and character limits; and readable scrambling
for websites that require uppercase letters, numbers, or symbols.

**SHA256SUMS.txt** contains SHA-256 checksums for all four downloads.
