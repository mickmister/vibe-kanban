use command_group::AsyncGroupChild;
use services::services::container::ContainerError;

pub(crate) async fn kill_process_group(child: &mut AsyncGroupChild) -> Result<(), ContainerError> {
    utils::process::kill_process_group(child)
        .await
        .map_err(ContainerError::KillFailed)
}

pub(crate) async fn kill_process_group_and_wait(
    child: &mut AsyncGroupChild,
) -> Result<(), ContainerError> {
    // The shared utility escalates signals and awaits the exact group leader;
    // successful return therefore confirms termination.
    let pgid = child
        .id()
        .ok_or(ContainerError::ExternalProcessUnresolved)?;
    kill_process_group(child).await?;
    wait_for_process_group_exit(pgid, std::time::Duration::from_secs(5)).await
}

#[cfg(unix)]
fn process_group_exists(pgid: u32) -> Result<bool, ContainerError> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pgid=,stat="])
        .output()
        .map_err(|_| ContainerError::ExternalProcessUnresolved)?;
    if !output.status.success() {
        return Err(ContainerError::ExternalProcessUnresolved);
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let mut parts = line.split_whitespace();
        parts.next().and_then(|value| value.parse::<u32>().ok()) == Some(pgid)
            && parts.next().is_some_and(|status| !status.starts_with('Z'))
    }))
}

#[cfg(all(test, unix))]
pub(crate) fn process_group_exists_for_test(pgid: u32) -> Result<bool, ContainerError> {
    process_group_exists(pgid)
}

#[cfg(not(unix))]
fn process_group_exists(_pgid: u32) -> Result<bool, ContainerError> {
    Err(ContainerError::ExternalProcessUnresolved)
}

async fn wait_for_process_group_exit(
    pgid: u32,
    timeout: std::time::Duration,
) -> Result<(), ContainerError> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if !process_group_exists(pgid)? {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(ContainerError::ExternalProcessUnresolved)
}

pub(crate) async fn terminate_process_group_by_id(pgid: u32) -> Result<(), ContainerError> {
    #[cfg(unix)]
    {
        use nix::{
            errno::Errno,
            sys::signal::{Signal, kill},
            unistd::Pid,
        };
        let group = Pid::from_raw(-(pgid as i32));
        match kill(group, Some(Signal::SIGTERM)) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(_) => return Err(ContainerError::ExternalProcessUnresolved),
        }
        if wait_for_process_group_exit(pgid, std::time::Duration::from_secs(2))
            .await
            .is_ok()
        {
            return Ok(());
        }
        match kill(group, Some(Signal::SIGKILL)) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(_) => return Err(ContainerError::ExternalProcessUnresolved),
        }
        return wait_for_process_group_exit(pgid, std::time::Duration::from_secs(5)).await;
    }
    #[cfg(not(unix))]
    Err(ContainerError::ExternalProcessUnresolved)
}
