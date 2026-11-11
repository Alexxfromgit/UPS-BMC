fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI failed to compile");

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
