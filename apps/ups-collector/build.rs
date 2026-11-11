fn main() {
    // Executable icon and version info on Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../ups-bmc/ui/icons/ups-bmc.ico")
            .set("ProductName", "UPS-BMC")
            .set("FileDescription", "UPS-BMC collector (background service)");
        res.compile().expect("compiling Windows resources");
    }
}
