use crate::{install::installed_launch, state::AppState};
use anyhow::Result;
use std::{path::PathBuf, process::Command, thread, time::Duration};
use vesper_format::validate_relative_path;
use vesper_install::Installer;

/// Launch an installed game with the launch target recorded in its manifest and
/// schedule the delayed backup cleanup. Returns the started executable.
pub fn launch_installed(install_dir: &str, state: &AppState) -> Result<String> {
    let launch = installed_launch(install_dir, state);
    start(
        install_dir,
        &launch.executable,
        &launch.args,
        launch.working_dir.as_deref(),
    )?;
    Ok(launch.executable)
}

/// Launch a named executable inside an installation and schedule the delayed backup cleanup.
pub fn launch_game(install_dir: &str, executable: &str, args: &[String]) -> Result<()> {
    start(install_dir, executable, args, None)
}

fn start(
    install_dir: &str,
    executable: &str,
    args: &[String],
    working_dir: Option<&str>,
) -> Result<()> {
    validate_relative_path(executable)?;
    let root = PathBuf::from(install_dir);
    let program = root.join(executable.replace('/', std::path::MAIN_SEPARATOR_STR));
    let cwd = match working_dir {
        Some(dir) => {
            validate_relative_path(dir)?;
            root.join(dir.replace('/', std::path::MAIN_SEPARATOR_STR))
        }
        None => root.clone(),
    };
    let mut command = Command::new(program);
    command.current_dir(cwd).args(args);
    let mut child = command.spawn()?;
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(60));
        let _ = child.try_wait();
        let _ = Installer::new(root).cleanup_backup();
    });
    Ok(())
}
