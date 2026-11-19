use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use vesper_format::Manifest;

fn pack(input: &Path, output: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["pack", "--input"])
        .arg(input)
        .arg("--output")
        .arg(output)
        .args(["--app-id", "test.pack", "--platform", "windows-x64"])
        .args(["--version", "1.0.0"])
        .args(extra)
        .output()
        .unwrap()
}

fn launch_of(output: &Path) -> Option<String> {
    let manifest = Manifest::decode(&fs::read(output.join("manifest.cbor.zst")).unwrap()).unwrap();
    manifest.install.launch.map(|launch| launch.executable)
}

fn unity_build(root: &Path) {
    fs::create_dir_all(root.join("NoDoorsAllowed_Data/Plugins")).unwrap();
    fs::write(root.join("NoDoorsAllowed.exe"), b"game").unwrap();
    fs::write(root.join("UnityCrashHandler64.exe"), b"crash").unwrap();
    fs::write(root.join("UnityPlayer.dll"), b"player").unwrap();
    fs::write(
        root.join("NoDoorsAllowed_Data/Plugins/helper.exe"),
        b"helper",
    )
    .unwrap();
}

#[test]
fn pack_records_the_only_top_level_exe_as_launch_target() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("game");
    unity_build(&input);
    let output = temp.path().join("dist");
    let result = pack(&input, &output, &[]);
    assert!(result.status.success(), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("launch=NoDoorsAllowed.exe"));
    assert_eq!(launch_of(&output).as_deref(), Some("NoDoorsAllowed.exe"));
}

#[test]
fn pack_launch_option_overrides_and_must_name_a_packed_file() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("game");
    unity_build(&input);
    let output = temp.path().join("dist");
    let result = pack(
        &input,
        &output,
        &["--launch", r"NoDoorsAllowed_Data\Plugins\helper.exe"],
    );
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        launch_of(&output).as_deref(),
        Some("NoDoorsAllowed_Data/Plugins/helper.exe")
    );

    let missing = pack(
        &input,
        &temp.path().join("missing"),
        &["--launch", "game.exe"],
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("not a file in the packed build"));
}

#[test]
fn pack_leaves_launch_unset_when_the_executable_is_ambiguous() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("game");
    unity_build(&input);
    fs::write(input.join("Server.exe"), b"server").unwrap();
    let output = temp.path().join("dist");
    let result = pack(&input, &output, &[]);
    assert!(result.status.success(), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("launch=none"));
    assert_eq!(launch_of(&output), None);
}
