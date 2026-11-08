use std::process::Command;

#[test]
fn version_reports_package_and_twelve_digit_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        version.trim(),
        format!("vesper {}", env!("VESPER_CLI_VERSION"))
    );
    let sha = version
        .trim()
        .strip_prefix(&format!("vesper {} (", env!("CARGO_PKG_VERSION")))
        .unwrap()
        .strip_suffix(')')
        .unwrap();
    assert!(sha.len() == 12 && sha.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(output.stderr.is_empty());
}

#[test]
fn release_archive_uses_windows_system_tar_instead_of_path() {
    let script = include_str!("../../../scripts/build-vesper-release.ps1");
    assert!(script.contains("$systemTar = Join-Path $env:SystemRoot 'System32/tar.exe'"));
    assert!(script.contains("Test-Path -LiteralPath $systemTar -PathType Leaf"));
    assert!(script.contains("Invoke-Checked $systemTar @('-xf', $archive, '-C', $source)"));
    assert!(!script.contains("Invoke-Checked 'tar'"));
}
