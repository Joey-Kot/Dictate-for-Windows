fn main() {
    println!("cargo:rerun-if-changed=../../native/ffmpeg_bridge.c");
    println!("cargo:rerun-if-changed=../../native/ffmpeg_bridge.h");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    println!("cargo:rerun-if-changed=app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // Subclass helpers require Common Controls v6 at process startup.
    // Embed the manifest even when the optional icon is absent.
    let mut resource = winresource::WindowsResource::new();
    resource
        .set("ProductName", "Dictate for Windows")
        .set("FileDescription", "Dictate for Windows")
        .set("InternalName", "Dictate")
        .set("OriginalFilename", "Dictate.exe");
    resource.set_manifest_file("app.manifest");
    if std::path::Path::new("../../assets/icon.ico").exists() {
        resource.set_icon("../../assets/icon.ico");
    }
    resource
        .compile()
        .expect("failed to embed Windows application resources");
}
