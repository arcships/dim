//! Effective project MCP configuration, resolved against its authorized workspace.
use super::*;

impl SettingsService {
    pub(crate) fn mcp_effective(
        &self,
        source: SettingsSource,
        mut config: McpServerSettings,
    ) -> Result<McpServerSettings> {
        if let SettingsSource::ProjectMcp(root) = source {
            let workspace = self
                .projects
                .lock()
                .unwrap()
                .iter()
                .find(|(_, index)| **index == root)
                .map(|(path, _)| path.clone())
                .ok_or(SettingsError::Invalid("unauthorized project"))?;
            if let McpServerSettings::Stdio { cwd, .. } = &mut config {
                // Resolve relative directories against the workspace, not the daemon.
                let path = cwd
                    .as_ref()
                    .map_or(workspace.clone(), |cwd| workspace.join(cwd));
                *cwd = Some(
                    path.to_str()
                        .ok_or(SettingsError::Invalid("project cwd must be UTF-8"))?
                        .to_owned(),
                );
            }
        }
        Ok(config)
    }
}
