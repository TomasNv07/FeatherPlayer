// Builds the app icon into the exe. Without it the taskbar shows a blank icon: the app sets an
// explicit AppUserModelID ("FeatherPlayer.App", window::set_app_id), and Windows then takes the
// taskbar icon from the exe file, not from the window icon set at runtime.
// The .ico is made from pics\feather1.png by setup\make_icon.ps1.
fn main() {
    println!("cargo:rerun-if-changed=pics/feather_app.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("pics/feather_app.ico");
        res.set("FileDescription", "Feather Player");
        res.set("ProductName", "Feather Player");
        if let Err(e) = res.compile() {
            // a missing rc.exe (Windows SDK) must not stop the build: the exe just has no icon
            println!("cargo:warning=app icon not embedded: {e}");
        }
    }
}
