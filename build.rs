fn main() {
    const WINDOWS_RC: &str = "resources/branding/windows/bibleown.rc";
    const WINDOWS_ICON: &str = "resources/branding/windows/mob.ico";

    println!("cargo:rerun-if-changed={WINDOWS_RC}");
    println!("cargo:rerun-if-changed={WINDOWS_ICON}");

    // GPUI for Windows loads the window/taskbar icon from resource ID 1.
    // Checking Cargo's target variable also keeps cross-compilation working.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile(WINDOWS_RC, embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed the BibleOWN Windows icon");
    }
}
