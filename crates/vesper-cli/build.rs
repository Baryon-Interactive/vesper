use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=VESPER_GIT_SHA");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["HEAD", "refs", "packed-refs"] {
        let path = root.join(".git").join(name);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let sha = std::env::var("VESPER_GIT_SHA").unwrap_or_else(|_| {
        let output = Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .current_dir(&root)
            .output()
            .expect("set VESPER_GIT_SHA when building without Git");
        assert!(
            output.status.success(),
            "set VESPER_GIT_SHA when building archived source"
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    });
    assert!(
        sha.len() == 12 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "VESPER_GIT_SHA must be a 12-digit hexadecimal commit ID"
    );
    println!(
        "cargo:rustc-env=VESPER_CLI_VERSION={} ({sha})",
        std::env::var("CARGO_PKG_VERSION").unwrap()
    );
}
