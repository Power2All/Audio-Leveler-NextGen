fn main() {
    // Compile the Slint UI only when building the GUI (`--features gui`). The Fluent style
    // follows the operating system's dark/light setting.
    #[cfg(feature = "gui")]
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("compiling ui/app.slint");

    // Windows: give the .exe files the app icon (shown in Explorer and on the taskbar).
    // Needs rc.exe (MSVC) or windres (MinGW, also for cross-builds from Linux).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/windows/icon.ico");
        res.compile().expect("embedding the Windows icon");
    }
}
