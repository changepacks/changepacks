mod changepacks;
mod check;
mod config;
mod init;
mod publish;
/// Private `check --tree` renderer; intentionally not re-exported, so the
/// public `commands` surface is unchanged.
mod tree;
mod update;

use std::{
    collections::HashMap,
    hash::BuildHasher,
    path::{Path, PathBuf},
};

use anyhow::Result;
use changepacks_core::{ChangePackResultLog, Project, UpdateType};
use changepacks_utils::gen_changepack_result_map;

/// Render the `--format json` changepack-result payload shared by `check` and `update`.
///
/// Both commands must emit byte-identical JSON for the same inputs, so the
/// `gen_changepack_result_map` + `serde_json::to_string_pretty` pair lives here
/// instead of being open-coded twice. Generic over the hasher exactly like
/// `gen_changepack_result_map`, so `UpdatePlan`'s `Deref` target passes through
/// unchanged.
///
/// # Errors
/// Returns an error if building the result map or serializing it fails.
pub(crate) fn changepack_result_json<S: BuildHasher>(
    projects: &[&Project],
    repo_root_path: &Path,
    update_map: &HashMap<PathBuf, (UpdateType, Vec<ChangePackResultLog>), S>,
) -> Result<String> {
    Ok(serde_json::to_string_pretty(&gen_changepack_result_map(
        projects,
        repo_root_path,
        update_map,
    )?)?)
}

pub use changepacks::ChangepackArgs;
pub use changepacks::handle_changepack;
pub use changepacks::handle_changepack_with_prompter;
pub use check::CheckArgs;
pub use check::handle_check;
pub use config::ConfigArgs;
pub use config::handle_config;
pub use init::InitArgs;
pub use init::handle_init;
pub use publish::PublishArgs;
pub use publish::handle_publish;
pub use publish::handle_publish_with_prompter;
pub use update::UpdateArgs;
pub use update::handle_update;
pub use update::handle_update_with_prompter;
