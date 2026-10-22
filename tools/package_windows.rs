//! Compatibility command for packaging Windows from Windows or WSL.
#[path = "package_release.rs"]
mod packaging;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    packaging::main()
}
