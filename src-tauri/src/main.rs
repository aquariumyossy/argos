// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Extraction runs in this same exe so a parser abort, stack overflow, or
    // job-object OOM kills only the child. Checked before Tauri starts.
    if std::env::args().nth(1).as_deref() == Some("--extract-worker") {
        argos_lib::extract_host::serve();
        return;
    }
    argos_lib::run()
}
