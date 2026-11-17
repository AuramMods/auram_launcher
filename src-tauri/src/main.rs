#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    auram_core::java_bridge::entry();
    auram_core::run();
}
