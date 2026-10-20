fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/four-words.ico");

    // Build scripts run on the host, including when cross-compiling for Windows.
    #[cfg(feature = "gui")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/four-words.ico")
            .set("ProductName", "Four Words")
            .set("FileDescription", "Four Words")
            .compile()
            .expect("failed to embed the Windows application icon");
    }
}
