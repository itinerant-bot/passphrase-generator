//! Build desktop releases, including dependency and dictionary licenses.
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use zip::write::SimpleFileOptions;

pub fn main() -> Result<(), Box<dyn Error>> {
    let default_target = if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }
    } else if std::env::current_exe()?
        .file_stem()
        .is_some_and(|name| name == "package-windows")
    {
        "x86_64-pc-windows-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| default_target.into());
    let platform = match target.as_str() {
        "x86_64-pc-windows-gnu" | "x86_64-pc-windows-msvc" => "windows-x64",
        "x86_64-unknown-linux-gnu" => "linux-x64",
        "aarch64-apple-darwin" => "macos-arm64",
        "x86_64-apple-darwin" => "macos-x64",
        _ => return Err("Unsupported release target.".into()),
    };
    let windows = platform.starts_with("windows");
    let macos = platform.starts_with("macos");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut build_command = Command::new("cargo");
    build_command.current_dir(root).args([
        "build",
        "--release",
        "--locked",
        "--features",
        "gui",
        "--target",
        &target,
        "--bin",
        "four-words-gui",
        "--bin",
        "four-words",
    ]);
    if !cfg!(windows) && target == "x86_64-pc-windows-gnu" {
        build_command.env(
            "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
            "x86_64-w64-mingw32-gcc",
        );
    }
    let status = build_command.status()?;
    if !status.success() {
        return Err("Release build failed.".into());
    }

    let metadata = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--features",
            "gui",
            "--filter-platform",
            &target,
        ])
        .output()?;
    if !metadata.status.success() {
        return Err("Could not read dependency licenses.".into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let build = Path::new(
        metadata["target_directory"]
            .as_str()
            .ok_or("Missing target directory")?,
    )
    .join(&target)
    .join("release");
    let dist = root.join("dist");
    let folder_name = format!("four-words-{}-{platform}", env!("CARGO_PKG_VERSION"));
    let folder = dist.join(&folder_name);
    fs::create_dir_all(&folder)?;
    let instructions = if windows {
        fs::copy(
            build.join("four-words-gui.exe"),
            folder.join("Four Words.exe"),
        )?;
        fs::copy(build.join("four-words.exe"), folder.join("four-words.exe"))?;
        "Double-click Four Words.exe."
    } else if macos {
        let app = folder.join("Four Words.app");
        let contents = app.join("Contents");
        fs::create_dir_all(contents.join("MacOS"))?;
        fs::copy(
            build.join("four-words-gui"),
            contents.join("MacOS/four-words-gui"),
        )?;
        fs::write(contents.join("Info.plist"), macos_plist())?;
        if !Command::new("codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(&app)
            .status()?
            .success()
        {
            return Err("Could not ad-hoc sign macOS app.".into());
        }
        if !Command::new("codesign")
            .args(["--verify", "--strict"])
            .arg(&app)
            .status()?
            .success()
        {
            return Err("macOS app signature verification failed.".into());
        }
        fs::copy(build.join("four-words"), folder.join("four-words"))?;
        "Open Four Words.app (you can drag it to Applications).\nIf macOS blocks it, open System Settings > Privacy & Security > Open Anyway.\nThis app is not Developer ID signed or notarized."
    } else {
        fs::copy(build.join("four-words-gui"), folder.join("four-words-gui"))?;
        fs::copy(build.join("four-words"), folder.join("four-words"))?;
        "Run ./four-words-gui from this folder.\nRequires Ubuntu 24.04 or a compatible newer Linux distribution, with a desktop and OpenGL.\nOn Ubuntu: sudo apt install libxkbcommon0 libwayland-client0 libegl1 libgl1 libxcb1"
    };
    fs::copy(root.join("LICENSE"), folder.join("LICENSE"))?;
    fs::write(
        folder.join("START HERE.txt"),
        format!(
            "FOUR WORDS\n\n{instructions}\n\nChoose languages and password limits, then click Copy password.\nClick New password for another suggestion.\n\nThe command-line tool and license notices are also included.\nNo Rust, WSL, or internet connection is needed to run the app.\n"
        ),
    )?;
    let licenses = folder.join("licenses");
    fs::create_dir_all(&licenses)?;
    for entry in fs::read_dir(root.join("wordlists/licenses"))? {
        let entry = entry?;
        fs::copy(entry.path(), licenses.join(entry.file_name()))?;
    }
    fs::copy(
        root.join("wordlists/README.md"),
        licenses.join("dictionary-sources.md"),
    )?;

    let mut credits = String::from("Four Words — third-party software\n\n");
    for package in metadata["packages"].as_array().ok_or("Missing packages")? {
        if package["name"] == "four-words" {
            continue;
        }
        let name = package["name"].as_str().ok_or("Missing package name")?;
        let version = package["version"]
            .as_str()
            .ok_or("Missing package version")?;
        let license = package["license"]
            .as_str()
            .unwrap_or("See included license files");
        credits.push_str(&format!("{name} {version}: {license}\n"));
        let source = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("Missing manifest")?,
        )
        .parent()
        .ok_or("Missing crate directory")?;
        let destination = licenses.join(format!("{name}-{version}"));
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let filename = entry.file_name();
            let upper = filename.to_string_lossy().to_ascii_uppercase();
            if entry.file_type()?.is_file()
                && ["LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"]
                    .iter()
                    .any(|prefix| upper.starts_with(prefix))
            {
                fs::create_dir_all(&destination)?;
                fs::copy(entry.path(), destination.join(filename))?;
            }
        }
        if let Some(filename) = package["license_file"].as_str() {
            let file = source.join(filename);
            if file.is_file() {
                fs::create_dir_all(&destination)?;
                fs::copy(
                    &file,
                    destination.join(file.file_name().ok_or("Missing license filename")?),
                )?;
            }
        }
    }
    fs::write(licenses.join("THIRD-PARTY.txt"), credits)?;
    let archive = dist.join(format!("{folder_name}.zip"));
    let mut zip = zip::ZipWriter::new(File::create(&archive)?);
    add_directory(&mut zip, &folder, &dist)?;
    zip.finish()?;
    println!("Release package: {}", archive.display());
    Ok(())
}

fn add_directory(
    zip: &mut zip::ZipWriter<File>,
    directory: &Path,
    base: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut entries: Vec<_> = fs::read_dir(directory)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            add_directory(zip, &path, base)?;
        } else {
            let name = path
                .strip_prefix(base)?
                .to_string_lossy()
                .replace('\\', "/");
            let executable = matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("four-words" | "four-words-gui")
            );
            zip.start_file(
                name,
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(if executable { 0o755 } else { 0o644 }),
            )?;
            let mut bytes = Vec::new();
            File::open(path)?.read_to_end(&mut bytes)?;
            zip.write_all(&bytes)?;
        }
    }
    Ok(())
}

fn macos_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Four Words</string>
<key>CFBundleDisplayName</key><string>Four Words</string>
<key>CFBundleIdentifier</key><string>com.itinerant.four-words</string>
<key>CFBundleExecutable</key><string>four-words-gui</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{version}</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_preserves_app_structure_and_executable_permissions() -> Result<(), Box<dyn Error>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("four-words-package-{}-{nonce}", std::process::id()));
        let folder = base.join("release");
        let executable = folder.join("Four Words.app/Contents/MacOS");
        fs::create_dir_all(&executable)?;
        fs::write(executable.join("four-words-gui"), b"gui")?;
        fs::write(folder.join("four-words"), b"cli")?;
        fs::write(folder.join("START HERE.txt"), b"instructions")?;
        let archive_path = base.join("release.zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive_path)?);
        add_directory(&mut writer, &folder, &base)?;
        writer.finish()?;
        let mut archive = zip::ZipArchive::new(File::open(&archive_path)?)?;
        for name in [
            "release/Four Words.app/Contents/MacOS/four-words-gui",
            "release/four-words",
        ] {
            assert_eq!(archive.by_name(name)?.unix_mode().unwrap() & 0o777, 0o755);
        }
        let mut instructions = archive.by_name("release/START HERE.txt")?;
        assert_eq!(instructions.unix_mode().unwrap() & 0o777, 0o644);
        let mut contents = String::new();
        instructions.read_to_string(&mut contents)?;
        assert_eq!(contents, "instructions");
        drop(instructions);
        drop(archive);
        fs::remove_dir_all(base)?;
        Ok(())
    }
}
