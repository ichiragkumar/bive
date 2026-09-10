fn main() {
    // tauri-build requires the tauri crate in the graph; without the feature the
    // crate is a plain library (bridge/notify/tray/ui_state) and must build bare.
    #[cfg(feature = "tauri")]
    tauri_build::build();
}
