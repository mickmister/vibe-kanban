//! Coding-agent process sandboxing helpers.
//!
//! MVP scope: these helpers wrap coding-agent executor processes only. Setup,
//! cleanup, script, dev-server, and archive processes are deliberately outside
//! the bwrap sandbox MVP and should be audited separately before inclusion.

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use ts_rs::TS;
use workspace_utils::shell::resolve_executable_path;

use crate::{command::CmdOverrides, env::ExecutionEnv, executors::ExecutorError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
pub enum SandboxBackend {
    Bwrap,
    #[serde(alias = "sandbox-exec")]
    SandboxExec,
}

impl Default for SandboxBackend {
    fn default() -> Self {
        Self::Bwrap
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[ts(use_ts_enum)]
pub enum SandboxNetworkMode {
    Inherit,
    None,
}

impl Default for SandboxNetworkMode {
    fn default() -> Self {
        Self::Inherit
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, JsonSchema)]
pub struct SandboxMount {
    pub host_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, JsonSchema)]
pub struct AgentSandboxConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub backend: SandboxBackend,
    #[serde(default)]
    pub network: SandboxNetworkMode,
    #[serde(default)]
    pub readonly_paths: Vec<SandboxMount>,
    #[serde(default)]
    pub writable_paths: Vec<SandboxMount>,
    #[serde(default = "default_readonly_repo_paths")]
    pub readonly_repo_paths: Vec<String>,
    #[serde(default)]
    pub auth_mounts: Vec<SandboxMount>,
    #[serde(default = "default_env_allowlist")]
    pub env_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_home: Option<String>,
}

impl Default for AgentSandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: SandboxBackend::default(),
            network: SandboxNetworkMode::default(),
            readonly_paths: Vec::new(),
            writable_paths: Vec::new(),
            readonly_repo_paths: default_readonly_repo_paths(),
            auth_mounts: Vec::new(),
            env_allowlist: default_env_allowlist(),
            sandbox_home: None,
        }
    }
}

fn default_readonly_repo_paths() -> Vec<String> {
    vec!["node_modules".to_string()]
}

fn default_env_allowlist() -> Vec<String> {
    [
        "PATH",
        "TERM",
        "LANG",
        "LC_ALL",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[derive(Debug, Clone)]
pub struct PreparedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub current_dir: PathBuf,
    pub env: HashMap<String, String>,
    pub clear_env: bool,
}

impl PreparedCommand {
    pub fn apply_env_to_command(&self, command: &mut Command) {
        if self.clear_env {
            command.env_clear();
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
    }
}

pub async fn prepare_agent_command(
    program_path: PathBuf,
    args: Vec<String>,
    current_dir: &Path,
    env: &ExecutionEnv,
    cmd_overrides: &CmdOverrides,
) -> Result<PreparedCommand, ExecutorError> {
    let sandbox = env.sandbox.as_ref().filter(|cfg| cfg.enabled);
    let Some(config) = sandbox else {
        return Ok(PreparedCommand {
            program: program_path,
            args,
            current_dir: current_dir.to_path_buf(),
            env: merged_env(env, cmd_overrides),
            clear_env: false,
        });
    };

    validate_backend_available(config).await?;
    let workspace_root = env.repo_context.workspace_root.as_path();
    let resolved_cwd = resolve_workspace_subpath(
        workspace_root,
        current_dir.strip_prefix(workspace_root).ok(),
    )?;
    let sandbox_home = sandbox_home_path(config, workspace_root)?;
    tokio::fs::create_dir_all(&sandbox_home)
        .await
        .map_err(ExecutorError::Io)?;

    let bwrap = resolve_executable_path("bwrap").await.ok_or_else(|| {
        ExecutorError::SandboxUnavailable("bwrap executable not found".to_string())
    })?;

    let mut bwrap_args = build_bwrap_args(
        config,
        workspace_root,
        &resolved_cwd,
        &sandbox_home,
        &program_path,
        &args,
        env,
    )?;
    if bwrap_args.first().is_some_and(|arg| arg == "bwrap") {
        bwrap_args.remove(0);
    }

    Ok(PreparedCommand {
        program: bwrap,
        args: bwrap_args,
        current_dir: resolved_cwd,
        env: sandbox_env(config, env, cmd_overrides, &sandbox_home),
        clear_env: true,
    })
}

pub fn configure_prepared_command(command: &mut Command, prepared: &PreparedCommand) {
    command
        .current_dir(&prepared.current_dir)
        .args(&prepared.args);
    prepared.apply_env_to_command(command);
}

pub fn resolve_workspace_subpath(
    workspace_root: &Path,
    subpath: Option<&Path>,
) -> Result<PathBuf, ExecutorError> {
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|err| ExecutorError::InvalidWorkingDir(format!("workspace root: {err}")))?;
    let subpath = subpath.unwrap_or_else(|| Path::new(""));

    if subpath.is_absolute() {
        return Err(ExecutorError::InvalidWorkingDir(
            "working_dir must be relative to the workspace".to_string(),
        ));
    }
    if subpath
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(ExecutorError::InvalidWorkingDir(
            "working_dir may not contain '..' or platform prefixes".to_string(),
        ));
    }

    let target = workspace_root.join(subpath);
    let canonical = target
        .canonicalize()
        .map_err(|err| ExecutorError::InvalidWorkingDir(format!("{}: {err}", target.display())))?;
    if !canonical.starts_with(&workspace_root) {
        return Err(ExecutorError::InvalidWorkingDir(format!(
            "working_dir escapes workspace: {}",
            subpath.display()
        )));
    }
    Ok(canonical)
}

async fn validate_backend_available(config: &AgentSandboxConfig) -> Result<(), ExecutorError> {
    match config.backend {
        SandboxBackend::Bwrap => {
            if !cfg!(target_os = "linux") {
                return Err(ExecutorError::SandboxUnavailable(
                    "bwrap sandboxing is only available on Linux".to_string(),
                ));
            }
        }
        SandboxBackend::SandboxExec => {
            return Err(ExecutorError::SandboxUnavailable(
                "sandbox-exec backend is not implemented yet".to_string(),
            ));
        }
    }
    Ok(())
}

fn sandbox_home_path(
    config: &AgentSandboxConfig,
    workspace_root: &Path,
) -> Result<PathBuf, ExecutorError> {
    match config.sandbox_home.as_deref() {
        Some(path) => resolve_workspace_subpath(workspace_root, Some(Path::new(path))),
        None => default_sandbox_home_path(workspace_root),
    }
}

fn default_sandbox_home_path(workspace_root: &Path) -> Result<PathBuf, ExecutorError> {
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|err| ExecutorError::InvalidWorkingDir(format!("workspace root: {err}")))?;
    let data_dir = dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .ok_or_else(|| {
            ExecutorError::SandboxUnavailable(
                "could not determine a local data directory for sandbox HOME".to_string(),
            )
        })?;
    let mut hasher = Sha256::new();
    hasher.update(workspace_root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    Ok(data_dir
        .join("vibe-kanban")
        .join("sandbox-homes")
        .join(&hash[..16]))
}

pub fn build_bwrap_args(
    config: &AgentSandboxConfig,
    workspace_root: &Path,
    current_dir: &Path,
    sandbox_home: &Path,
    program_path: &Path,
    program_args: &[String],
    env: &ExecutionEnv,
) -> Result<Vec<String>, ExecutorError> {
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|err| ExecutorError::InvalidSandboxConfig(format!("workspace root: {err}")))?;
    let mut args = vec!["bwrap".to_string(), "--unshare-all".to_string()];
    if matches!(config.network, SandboxNetworkMode::Inherit) {
        args.push("--share-net".to_string());
    }
    args.extend(
        [
            "--die-with-parent",
            "--new-session",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
        ]
        .into_iter()
        .map(str::to_string),
    );

    for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"] {
        args.push("--ro-bind-try".to_string());
        args.push(path.to_string());
        args.push(path.to_string());
    }

    push_bind(&mut args, "--bind", &workspace_root, &workspace_root);
    push_bind(&mut args, "--bind", sandbox_home, sandbox_home);

    for mount in &config.writable_paths {
        let (src, dest) = canonical_mount(mount)?;
        push_bind(&mut args, "--bind", &src, &dest);
    }

    for repo_name in &env.repo_context.repo_names {
        for rel in &config.readonly_repo_paths {
            let rel_path = Path::new(rel);
            if rel_path.is_absolute()
                || rel_path
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
            {
                return Err(ExecutorError::InvalidSandboxConfig(format!(
                    "invalid readonly repo path: {rel}"
                )));
            }
            let path = workspace_root.join(repo_name).join(rel_path);
            if path.exists() {
                reject_symlink_components(
                    &workspace_root,
                    Path::new(repo_name).join(rel_path).as_path(),
                )?;
                let canonical = path.canonicalize().map_err(|err| {
                    ExecutorError::InvalidSandboxConfig(format!("{}: {err}", path.display()))
                })?;
                if !canonical.starts_with(&workspace_root) {
                    return Err(ExecutorError::InvalidSandboxConfig(format!(
                        "readonly repo path escapes workspace: {}",
                        path.display()
                    )));
                }
                push_bind(&mut args, "--ro-bind", &canonical, &path);
            }
        }
    }

    for mount in config
        .readonly_paths
        .iter()
        .chain(config.auth_mounts.iter())
    {
        let (src, dest) = canonical_mount(mount)?;
        push_bind(&mut args, "--ro-bind", &src, &dest);
    }

    args.push("--chdir".to_string());
    args.push(current_dir.to_string_lossy().to_string());
    args.push("--setenv".to_string());
    args.push("HOME".to_string());
    args.push(sandbox_home.to_string_lossy().to_string());
    args.push("--".to_string());
    args.push(program_path.to_string_lossy().to_string());
    args.extend(program_args.iter().cloned());
    Ok(args)
}

fn reject_symlink_components(
    workspace_root: &Path,
    relative_path: &Path,
) -> Result<(), ExecutorError> {
    let mut cursor = workspace_root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(name) = component else {
            return Err(ExecutorError::InvalidSandboxConfig(format!(
                "invalid readonly repo path component: {}",
                relative_path.display()
            )));
        };
        cursor.push(name);
        let metadata = std::fs::symlink_metadata(&cursor).map_err(|err| {
            ExecutorError::InvalidSandboxConfig(format!("{}: {err}", cursor.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ExecutorError::InvalidSandboxConfig(format!(
                "readonly repo path may not contain symlinks: {}",
                cursor.display()
            )));
        }
    }
    Ok(())
}

fn push_bind(args: &mut Vec<String>, flag: &str, src: &Path, dest: &Path) {
    args.push(flag.to_string());
    args.push(src.to_string_lossy().to_string());
    args.push(dest.to_string_lossy().to_string());
}

fn canonical_mount(mount: &SandboxMount) -> Result<(PathBuf, PathBuf), ExecutorError> {
    let src = PathBuf::from(&mount.host_path)
        .canonicalize()
        .map_err(|err| {
            ExecutorError::InvalidSandboxConfig(format!("{}: {err}", mount.host_path))
        })?;
    let dest = mount
        .sandbox_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| src.clone());
    if !dest.is_absolute() {
        return Err(ExecutorError::InvalidSandboxConfig(format!(
            "sandbox_path must be absolute: {}",
            dest.display()
        )));
    }
    Ok((src, dest))
}

fn merged_env(env: &ExecutionEnv, cmd_overrides: &CmdOverrides) -> HashMap<String, String> {
    let mut out = env.vars.clone();
    if let Some(profile_env) = &cmd_overrides.env {
        out.extend(profile_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    out
}

fn sandbox_env(
    config: &AgentSandboxConfig,
    env: &ExecutionEnv,
    cmd_overrides: &CmdOverrides,
    sandbox_home: &Path,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for key in &config.env_allowlist {
        if let Ok(value) = std::env::var(key) {
            out.insert(key.clone(), value);
        }
    }
    out.insert(
        "HOME".to_string(),
        sandbox_home.to_string_lossy().to_string(),
    );
    out.extend(env.vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    if let Some(profile_env) = &cmd_overrides.env {
        out.extend(profile_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::RepoContext;

    #[test]
    fn rejects_absolute_and_parent_working_dirs() {
        let root = tempfile::tempdir().unwrap();
        assert!(resolve_workspace_subpath(root.path(), Some(Path::new("/tmp"))).is_err());
        assert!(resolve_workspace_subpath(root.path(), Some(Path::new("../x"))).is_err());
    }

    #[test]
    fn rejects_symlink_escape_and_accepts_repo_subdir() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("repo")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("repo/link")).unwrap();
        assert_eq!(
            resolve_workspace_subpath(root.path(), Some(Path::new("repo"))).unwrap(),
            root.path().join("repo").canonicalize().unwrap()
        );
        #[cfg(unix)]
        assert!(resolve_workspace_subpath(root.path(), Some(Path::new("repo/link"))).is_err());
    }

    #[test]
    fn bwrap_args_mount_workspace_before_readonly_repo_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("repo/node_modules")).unwrap();
        let env = ExecutionEnv {
            vars: HashMap::new(),
            repo_context: RepoContext::new(root.path().to_path_buf(), vec!["repo".to_string()]),
            commit_reminder: false,
            commit_reminder_prompt: String::new(),
            sandbox: None,
        };
        let cfg = AgentSandboxConfig {
            enabled: true,
            ..Default::default()
        };
        let args = build_bwrap_args(
            &cfg,
            root.path(),
            &root.path().join("repo"),
            &root.path().join(".vibe/sandbox-home"),
            Path::new("/usr/bin/true"),
            &[],
            &env,
        )
        .unwrap();
        let root_canonical = root.path().canonicalize().unwrap();
        let workspace_bind = args
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == root_canonical.to_string_lossy());
        let nm_bind = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1].ends_with("repo/node_modules"));
        assert!(workspace_bind.unwrap() < nm_bind.unwrap());
        assert!(args.contains(&"--new-session".to_string()));
        assert!(args.contains(&"--die-with-parent".to_string()));
        assert!(args.contains(&"--proc".to_string()));
        assert!(args.contains(&"--dev".to_string()));
        assert!(args.contains(&"--tmpfs".to_string()));
        assert!(args.contains(&"--share-net".to_string()));
    }

    #[test]
    fn bwrap_omits_share_net_when_disabled() {
        let root = tempfile::tempdir().unwrap();
        let env = ExecutionEnv {
            vars: HashMap::new(),
            repo_context: RepoContext::new(root.path().to_path_buf(), vec![]),
            commit_reminder: false,
            commit_reminder_prompt: String::new(),
            sandbox: None,
        };
        let cfg = AgentSandboxConfig {
            enabled: true,
            network: SandboxNetworkMode::None,
            ..Default::default()
        };
        let args = build_bwrap_args(
            &cfg,
            root.path(),
            root.path(),
            &root.path().join(".home"),
            Path::new("/bin/true"),
            &[],
            &env,
        )
        .unwrap();
        assert!(!args.contains(&"--share-net".to_string()));
    }

    #[test]
    fn default_sandbox_home_is_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let home = sandbox_home_path(&AgentSandboxConfig::default(), root.path()).unwrap();
        assert!(!home.starts_with(root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn readonly_repo_path_rejects_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("repo")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("repo/node_modules")).unwrap();
        let env = ExecutionEnv {
            vars: HashMap::new(),
            repo_context: RepoContext::new(root.path().to_path_buf(), vec!["repo".to_string()]),
            commit_reminder: false,
            commit_reminder_prompt: String::new(),
            sandbox: None,
        };
        let cfg = AgentSandboxConfig {
            enabled: true,
            ..Default::default()
        };
        let err = build_bwrap_args(
            &cfg,
            root.path(),
            &root.path().join("repo"),
            &root.path().join(".home"),
            Path::new("/bin/true"),
            &[],
            &env,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("symlinks"));
    }

    #[test]
    fn every_coding_agent_spawn_path_uses_sandbox_prepare_helper() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (agent, file) in [
            ("Claude", "src/executors/claude.rs"),
            ("Codex", "src/executors/codex.rs"),
            ("Cursor", "src/executors/cursor.rs"),
            ("Amp", "src/executors/amp.rs"),
            ("Opencode", "src/executors/opencode.rs"),
            ("Droid", "src/executors/droid.rs"),
            (
                "Gemini/Qwen/Copilot ACP harness",
                "src/executors/acp/harness.rs",
            ),
        ] {
            let source = std::fs::read_to_string(manifest_dir.join(file)).unwrap();
            assert!(
                source.contains("prepare_agent_command"),
                "{agent} spawn path should use prepare_agent_command"
            );
        }

        #[cfg(feature = "qa-mode")]
        {
            let source =
                std::fs::read_to_string(manifest_dir.join("src/executors/qa_mock.rs")).unwrap();
            assert!(source.contains("prepare_agent_command"));
        }
    }
}
