#![allow(clippy::print_stdout)]

fn main() {
    println!("cargo:rerun-if-changed=../migrations");
}
