fn main() {
    // App icon (resource id 1, which gpui loads for the window class) for the .exe on Windows.
    #[cfg(windows)]
    embed_resource::compile("assets/clipbox.rc", embed_resource::NONE).manifest_optional().unwrap();
}
