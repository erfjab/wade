fn main() {
    // App icon (resource id 1) for the .exe and the tray.
    embed_resource::compile("assets/wade.rc", embed_resource::NONE).manifest_optional().unwrap();
}
