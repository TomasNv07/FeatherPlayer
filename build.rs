// Builds the app icon into the exe. Without it the taskbar shows a blank icon: the app sets an
// explicit AppUserModelID ("FeatherPlayer.App", window::set_app_id), and Windows then takes the
// taskbar icon from the exe file, not from the window icon set at runtime.
// The .ico is made from pics\feather1.png by setup\make_icon.ps1.
fn main() {
    println!("cargo:rerun-if-changed=pics/feather_app.ico");
    // The app's version for the update check (update::VERSION): build_installer.bat sets
    // FP_VERSION to the installer's version; other builds use the newest vX.Y.Z git tag.
    println!("cargo:rerun-if-env-changed=FP_VERSION");
    println!("cargo:rerun-if-changed=.git/refs/tags");
    let version = std::env::var("FP_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            let out = std::process::Command::new("git").args(["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*"]).output().ok()?;
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().trim_start_matches('v').to_owned())
        })
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=FP_VERSION={}", version.trim());
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
