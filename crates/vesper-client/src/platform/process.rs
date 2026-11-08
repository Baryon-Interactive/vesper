use anyhow::Result;
use std::{path::PathBuf, process::Command, thread, time::Duration};
use vesper_install::Installer;

/// Launch a game and schedule the delayed backup cleanup.
pub fn launch_game(install_dir: &str, executable: &str, args: &[String]) -> Result<()> {
    let root = PathBuf::from(install_dir);
    let program = root.join(executable.replace('/', std::path::MAIN_SEPARATOR_STR));
    let mut command = Command::new(program);
    command.current_dir(&root).args(args);
    let mut child = command.spawn()?;
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(60));
        let _ = child.try_wait();
        let _ = Installer::new(root).cleanup_backup();
    });
    Ok(())
}
