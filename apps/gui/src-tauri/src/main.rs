// No console window next to the GUI in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    crossagents_gui_lib::run();
}
