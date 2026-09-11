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
    kill_process_group(child).await?;
    Ok(())
}
