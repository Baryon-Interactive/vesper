use std::{fs, thread, time::Duration};

fn main() {
    let version = fs::read_to_string("VERSION.txt").unwrap_or_else(|_| "unknown".into());
    println!("Vesper demo game version {}", version.trim());
    println!("This process remains alive for 70 seconds so the launcher can verify backup cleanup.");
    thread::sleep(Duration::from_secs(70));
}
