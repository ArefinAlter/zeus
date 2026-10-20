//! The Zeus desktop executable.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    zeus_desktop::run();
}
