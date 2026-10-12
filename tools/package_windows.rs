//! Build a portable Windows release, including dependency and dictionary licenses.
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use zip::write::SimpleFileOptions;

fn main() -> Result<(), Box<dyn Error>> {
    let default_target = if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-pc-windows-gnu"
    };
    let target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| default_target.into());
    if !["x86_64-pc-windows-gnu", "x86_64-pc-windows-msvc"].contains(&target.as_str()) {
        return Err("Use x86_64-pc-windows-gnu or x86_64-pc-windows-msvc.".into());
    }
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
        return Err("Windows build failed.".into());
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
    let folder_name = format!("four-words-{}-windows-x64", env!("CARGO_PKG_VERSION"));
    let folder = dist.join(&folder_name);
    fs::create_dir_all(&folder)?;
    fs::copy(
        build.join("four-words-gui.exe"),
        folder.join("Four Words.exe"),
    )?;
    fs::copy(build.join("four-words.exe"), folder.join("four-words.exe"))?;
    fs::copy(root.join("LICENSE"), folder.join("LICENSE"))?;
    fs::write(
        folder.join("START HERE.txt"),
        "FOUR WORDS\r\n\r\nDouble-click Four Words.exe.\r\n\r\nAdjust the sliders, or type a number in the boxes beside them.\r\nTick 'Limit the number of words' if you want a word limit.\r\nClick 'Copy password' to copy it. Click 'New password' for a new one.\r\nClose the window when you are finished.\r\n\r\nNo installation or internet connection is needed.\r\n",
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
    println!("Portable Windows package: {}", archive.display());
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
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )?;
            let mut bytes = Vec::new();
            File::open(path)?.read_to_end(&mut bytes)?;
            zip.write_all(&bytes)?;
        }
    }
    Ok(())
}
