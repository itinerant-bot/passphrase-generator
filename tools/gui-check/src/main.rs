#[cfg(not(windows))]
fn main() {
    eprintln!("Run this checker on Windows with the GUI executable path.");
    std::process::exit(70);
}

#[cfg(windows)]
mod native;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}
