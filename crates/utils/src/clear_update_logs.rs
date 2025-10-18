use crate::get_changepack_dir;
use anyhow::Result;
use tokio::fs::{read_dir, remove_file};

// remove all update logs without confirmation
pub async fn clear_update_logs() -> Result<()> {
    let changepack_dir = get_changepack_dir()?;
    if !changepack_dir.exists() {
        return Ok(());
    }
    let mut entries = read_dir(&changepack_dir).await?;
    let mut update_logs = vec![];
    while let Some(file) = entries.next_entry().await? {
        if file.file_name().to_string_lossy() == "changepack.json" {
            continue;
        }
        update_logs.push(remove_file(file.path()));
    }
    futures::future::join_all(update_logs).await;
    Ok(())
}
