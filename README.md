# four-words

A Rust password generator with a desktop app for Windows, macOS, and Linux,
and a CLI. English, French, and German dictionaries are bundled. Builds require
Rust 1.88 or newer.

## Build

From the repository directory, build the GUI and CLI:

```sh
cargo build --release --locked
```

On macOS, install Xcode Command Line Tools (`xcode-select --install`) and
Rust 1.88 or newer. The same build command works on Apple Silicon and Intel
Macs, using the native Rust toolchain. macOS builds use native system libraries
and do not require X11, Wayland, or Linux.

## Run the executables

On Windows:

```powershell
.\target\release\four-words-gui.exe
.\target\release\four-words.exe --dict ef --len 63 --count 5
```

On macOS or Linux:

```sh
./target/release/four-words-gui
./target/release/four-words --dict ef --len 63 --count 5
```

The binaries contain their dictionaries. You can copy them elsewhere and run
them directly without Cargo, Rust, or the source tree. The GUI stays open until
you close it. Native Windows builds do not need Linux.

## Desktop app

The app follows the OS light or dark theme automatically, including changes
while it is open. Its Library of Babel icon combines an open book, an infinity
loop, and receding hexagonal arches. The icon is embedded in the GUI window and
in Windows executables, including portable releases; no external image files
are needed to run the app. Use the sliders or type into their number boxes. The
character limit defaults to **128**. Tick **Limit the number
of words** to enable the optional word cap. Choose your languages and either a
fixed hyphen or a space between words. Changes update the password and entropy.
**New password** makes another suggestion; **Copy password** copies it.

Entropy appears once, with the current password's character and word counts.
A single color shows the achieved entropy, blending between the range
midpoints and reaching violet at 96 bits:

| Entropy | Color | Classification |
| --- | --- | --- |
| Below 55 bits | Crimson | Insecure |
| 55 to below 65 bits | Amber | Standard consumer |
| 65 to below 80 bits | Emerald | Strong protection |
| 80 to below 96 bits | Sapphire | Master key grade |
| 96 bits and above | Violet | Password manager grade — what James uses |

The app opens with English selected and the word cap off. The GUI supports
4–128 characters and an optional cap of 1–25 words.

## CLI

```sh
./target/release/four-words --dict ef --len 63 --count 5
```

Entropy prints once per run on stderr. Each suggestion prints its password on
stdout and its actual length on stderr:

```text
Entropy: 69.28 bits
Length: 39 characters
```

Length includes separators. `--count` caps words; `--len` caps characters.
On Windows, use `.\target\release\four-words.exe` instead.

To build just the CLI without desktop dependencies:

```sh
cargo build --release --locked --no-default-features --bin four-words
```

## Install

```sh
cargo install --path . --locked
four-words --dict ef --len 63 --count 5
four-words --dict g --len 63 --separator " "
four-words --dict efg --len 63 --count 5 --suggestions 5
```

## Options

| Option | Meaning | Default |
| --- | --- | --- |
| `--dict` | `e` = English, `f` = French, `g` = German; combine letters | `efg` |
| `--len` | Maximum characters, including separators | `128` |
| `--count`, `-n` | Maximum words | Limited by `--len` |
| `--separator` | Fixed printable ASCII separator | `-` |
| `--suggestions` | Number of passwords to generate | `1` |

Words use only lowercase ASCII letters and have at least four characters.
Selected dictionaries are merged and deduplicated. The separator is fixed and
contributes zero entropy.

The generator samples uniformly over valid word sequences within both caps,
using exact counts and secure OS randomness. Dictionaries are embedded, so the
installed binary runs offline.

## Development

```sh
cargo test --locked
cargo fmt --check
cargo test --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
```

Build the portable Windows ZIP on Windows with the MSVC Rust toolchain:

```powershell
cargo build --release --locked --features packaging --bin package-windows
.\target\release\package-windows.exe
```

On Linux, install the `x86_64-pc-windows-gnu` Rust target and a MinGW-w64
cross-compiler, build the packager with the same Cargo command, and execute
`./target/release/package-windows`. The packager builds both Windows
executables and includes dictionary and dependency licenses. It writes a folder
and ZIP under `dist/`. The Windows builds use a static C runtime.

The app icon assets are in `assets/`: `four-words-source.png` is the generated
master, `four-words.png` is the GUI icon, and `four-words.ico` contains the Windows
icon sizes. The generation prompt is in `assets/icon-prompt.txt`. Windows GUI
builds embed the ICO through `build.rs`, using the Windows SDK resource compiler
on MSVC or MinGW-w64's `windres` on GNU. CLI-only builds with
`--no-default-features` do not need a resource compiler.

GUI measurements use a vendored `ui-observation` snapshot from commit
`db0ce917815a1f699a1f161b38677c5434f59c64`; its source digests are in
`vendor/ui-observation/vendor-authority.toml`. Observation is inactive during
ordinary use unless an observer supplies its inherited channel.

The independent Rust checker uses the observation channel and targeted Windows
UI Automation to verify presentation, visibility, editable values, slider
synchronization, copying, regeneration, and clean closure:

```sh
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --manifest-path tools/gui-check/Cargo.toml --locked --offline \
  --target x86_64-pc-windows-gnu
tools/gui-check/target/x86_64-pc-windows-gnu/debug/four-words-gui-check.exe \
  'C:\path\to\Four Words.exe'
```

Dictionary sources, licenses, and import instructions:
[wordlists/README.md](wordlists/README.md).

## License

Project code is licensed under the [MIT License](LICENSE), copyright
2026 James Lear. Bundled dictionaries, dependencies, and fonts retain their
original licenses and notices; see [dictionary attribution](wordlists/README.md)
and the notices included with vendored code.
