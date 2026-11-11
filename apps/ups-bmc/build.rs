fn main() {
    // Dark Fluent widgets match the app's dark theme on every OS setting.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI failed to compile");

    // Executable icon and version info on Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("ui/icons/ups-bmc.ico")
            .set("ProductName", "UPS-BMC")
            .set(
                "FileDescription",
                "UPS-BMC - UPS & Battery Monitoring Center",
            );
        res.compile().expect("compiling Windows resources");
    }
}
