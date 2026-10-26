//! Embeds the app icon into the Windows executable; other platforms take it from the bundle or
//! from the window icon set at runtime.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/strata.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/strata.ico");
        res.compile().expect("embed Windows icon");
    }
}
