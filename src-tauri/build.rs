fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&[
                "enter_flight_mode",
                "prepare_web_bridge",
                "exit_flight_mode",
                "new_conversation",
                "load_conversation_list",
                "select_conversation",
                "send_message",
                "update_command_draft",
                "select_command_option",
                "update_command_suffix",
                "clear_command_selection",
                "set_split_view",
                "relay_stream_event",
                "import_cookies",
                "export_cookies",
            ]),
        ),
    )
    .expect("failed to build Tauri application")
}
