// Release builds hide the console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    paxile_app_lib::run();
}
