use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use db::models::{
    project::Project,
    repo::{Repo, UpdateRepo},
    scratch::{
        ProjectRepoDefaultsData, Scratch, ScratchPayload, ScratchType, UiPreferencesData,
        UpdateScratch,
    },
    tag::{CreateTag, Tag},
};
use executors::profile::ExecutorConfigs;
use git::GitService;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use services::services::config::{Config, load_config_from_file, save_config_to_file};
use sqlx::SqlitePool;
use url::Url;
use utils::{assets, path::expand_tilde};
use uuid::Uuid;

const BACKUP_SCHEMA_VERSION: u32 = 1;
const DEFAULT_REPO_ROOT: &str = "~/repos";
const UI_PREFERENCES_ID: Uuid = Uuid::from_u128(1);

#[derive(Debug, Clone)]
pub struct BackupPaths {
    pub config_path: PathBuf,
    pub profiles_path: PathBuf,
    pub default_repo_root: PathBuf,
}

impl BackupPaths {
    pub fn from_assets() -> Self {
        Self {
            config_path: assets::config_path(),
            profiles_path: assets::profiles_path(),
            default_repo_root: expand_tilde(DEFAULT_REPO_ROOT),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibeKanbanBackupV1 {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    pub config: Config,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_profiles: Option<Value>,
    #[serde(default)]
    pub repos: Vec<BackupRepo>,
    #[serde(default)]
    pub projects: Vec<BackupProject>,
    #[serde(default)]
    pub tags: Vec<BackupTag>,
    #[serde(default)]
    pub preferences: BackupPreferences,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupRepo {
    pub source_id: Uuid,
    pub name: String,
    pub display_name: String,
    pub path_hint: PathBuf,
    pub clone: BackupRepoClone,
    pub config: BackupRepoConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupRepoClone {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub remotes: Vec<BackupGitRemote>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupGitRemote {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupRepoConfig {
    #[serde(default)]
    pub setup_script: Option<String>,
    #[serde(default)]
    pub cleanup_script: Option<String>,
    #[serde(default)]
    pub archive_script: Option<String>,
    #[serde(default)]
    pub copy_files: Option<String>,
    #[serde(default)]
    pub parallel_setup_script: bool,
    #[serde(default)]
    pub dev_server_script: Option<String>,
    #[serde(default)]
    pub default_target_branch: Option<String>,
    #[serde(default)]
    pub default_working_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupProject {
    pub source_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub remote_project_id: Option<Uuid>,
    #[serde(default)]
    pub default_agent_working_dir: Option<String>,
    #[serde(default)]
    pub repo_defaults: Vec<BackupProjectRepoDefault>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupProjectRepoDefault {
    pub repo_source_id: Uuid,
    pub target_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupTag {
    pub source_id: Uuid,
    pub tag_name: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_preferences: Option<UiPreferencesData>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportReport {
    pub repos_registered: usize,
    pub repos_cloned: usize,
    pub repos_skipped: usize,
    pub tags_imported: usize,
    pub projects_imported: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportedRepoAction {
    Registered,
    Cloned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoImportPathDecision {
    UseExistingHint,
    UseExistingDestination,
    Clone,
    SkipExistingNonRepoDestination,
    SkipNoCloneUrl,
}

fn decide_repo_import_path(
    path_hint: &Path,
    destination: &Path,
    clone_url: Option<&str>,
    git: &GitService,
) -> RepoImportPathDecision {
    if git.is_repo_openable(path_hint) {
        return RepoImportPathDecision::UseExistingHint;
    }

    if destination.exists() {
        return if git.is_repo_openable(destination) {
            RepoImportPathDecision::UseExistingDestination
        } else {
            RepoImportPathDecision::SkipExistingNonRepoDestination
        };
    }

    if clone_url.is_some() {
        RepoImportPathDecision::Clone
    } else {
        RepoImportPathDecision::SkipNoCloneUrl
    }
}

pub fn sanitize_remote_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_string();
    };

    if !url.username().is_empty() || url.password().is_some() {
        let _ = url.set_username("");
        let _ = url.set_password(None);
    }

    url.to_string()
}

fn load_profiles_json(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read profiles from {}", path.display()))?;
    let value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse profiles JSON from {}", path.display()))?;
    Ok(Some(redact_sensitive_json(value)))
}

fn redact_sensitive_json(mut value: Value) -> Value {
    redact_sensitive_json_in_place(&mut value);
    value
}

fn redact_sensitive_json_in_place(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                if key.eq_ignore_ascii_case("env") {
                    map.remove(&key);
                    continue;
                }

                if is_sensitive_key(&key) {
                    map.insert(key, Value::String("__REDACTED__".to_string()));
                    continue;
                }

                if let Some(child) = map.get_mut(&key) {
                    redact_sensitive_json_in_place(child);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_sensitive_json_in_place(value);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.ends_with("_key")
}

fn write_profiles_json(path: &Path, value: &Option<Value>) -> Result<()> {
    match value {
        Some(value) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
            ExecutorConfigs::reload();
        }
        None if path.exists() => {
            std::fs::remove_file(path)?;
            ExecutorConfigs::reload();
        }
        None => {}
    }
    Ok(())
}

pub async fn export_backup(
    pool: &SqlitePool,
    paths: &BackupPaths,
) -> Result<VibeKanbanBackupV1> {
    let config = portable_config(load_config_from_file(&paths.config_path).await);
    let executor_profiles = load_profiles_json(&paths.profiles_path)?;
    let git = GitService::new();

    let repos = Repo::list_all(pool)
        .await?
        .into_iter()
        .map(|repo| backup_repo_from_model(&git, repo))
        .collect::<Vec<_>>();

    let project_defaults = export_project_repo_defaults(pool).await?;
    let projects = Project::find_all(pool)
        .await?
        .into_iter()
        .map(|project| BackupProject {
            source_id: project.id,
            name: project.name,
            remote_project_id: project.remote_project_id,
            default_agent_working_dir: project.default_agent_working_dir,
            repo_defaults: project_defaults
                .get(&project.id)
                .cloned()
                .unwrap_or_default(),
        })
        .collect();

    let tags = Tag::find_all(pool)
        .await?
        .into_iter()
        .map(|tag| BackupTag {
            source_id: tag.id,
            tag_name: tag.tag_name,
            content: tag.content,
        })
        .collect();

    Ok(VibeKanbanBackupV1 {
        schema_version: BACKUP_SCHEMA_VERSION,
        exported_at: Utc::now(),
        app_version: option_env!("CARGO_PKG_VERSION").map(str::to_string),
        config,
        executor_profiles,
        repos,
        projects,
        tags,
        preferences: BackupPreferences {
            ui_preferences: export_ui_preferences(pool).await?,
        },
    })
}

fn portable_config(mut config: Config) -> Config {
    config.github.pat = None;
    config.github.oauth_token = None;
    config
}

fn backup_repo_from_model(git: &GitService, repo: Repo) -> BackupRepo {
    let remotes = git
        .list_remotes(&repo.path)
        .unwrap_or_default()
        .into_iter()
        .map(|remote| BackupGitRemote {
            name: remote.name,
            url: sanitize_remote_url(&remote.url),
        })
        .collect::<Vec<_>>();
    let preferred_remote = remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .or_else(|| remotes.first());

    BackupRepo {
        source_id: repo.id,
        name: repo.name,
        display_name: repo.display_name,
        path_hint: repo.path,
        clone: BackupRepoClone {
            preferred_remote: preferred_remote.map(|remote| remote.name.clone()),
            url: preferred_remote.map(|remote| remote.url.clone()),
            remotes,
        },
        config: BackupRepoConfig {
            setup_script: repo.setup_script,
            cleanup_script: repo.cleanup_script,
            archive_script: repo.archive_script,
            copy_files: repo.copy_files,
            parallel_setup_script: repo.parallel_setup_script,
            dev_server_script: repo.dev_server_script,
            default_target_branch: repo.default_target_branch,
            default_working_dir: repo.default_working_dir,
        },
    }
}

async fn export_project_repo_defaults(
    pool: &SqlitePool,
) -> Result<HashMap<Uuid, Vec<BackupProjectRepoDefault>>> {
    let scratches = Scratch::find_all(pool).await?;
    let mut defaults = HashMap::new();
    for scratch in scratches {
        if let ScratchPayload::ProjectRepoDefaults(ProjectRepoDefaultsData { repos }) =
            scratch.payload
        {
            defaults.insert(
                scratch.id,
                repos
                    .into_iter()
                    .map(|repo| BackupProjectRepoDefault {
                        repo_source_id: repo.repo_id,
                        target_branch: repo.target_branch,
                    })
                    .collect(),
            );
        }
    }
    Ok(defaults)
}

async fn export_ui_preferences(pool: &SqlitePool) -> Result<Option<UiPreferencesData>> {
    Ok(Scratch::find_all(pool)
        .await?
        .into_iter()
        .find_map(|scratch| match scratch.payload {
            ScratchPayload::UiPreferences(preferences) => Some(preferences),
            _ => None,
        }))
}

pub async fn import_backup(
    pool: &SqlitePool,
    paths: &BackupPaths,
    backup: &VibeKanbanBackupV1,
) -> Result<ImportReport> {
    if backup.schema_version != BACKUP_SCHEMA_VERSION {
        bail!(
            "unsupported backup schema version {}; expected {}",
            backup.schema_version,
            BACKUP_SCHEMA_VERSION
        );
    }

    save_config_to_file(&portable_config(backup.config.clone()), &paths.config_path).await?;
    let executor_profiles = backup.executor_profiles.clone().map(redact_sensitive_json);
    write_profiles_json(&paths.profiles_path, &executor_profiles)?;

    let git = GitService::new();
    let mut report = ImportReport::default();
    let mut repo_id_map = HashMap::new();

    for backup_repo in &backup.repos {
        match import_repo(pool, &git, &paths.default_repo_root, backup_repo).await? {
            Some((new_repo_id, action)) => {
                repo_id_map.insert(backup_repo.source_id, new_repo_id);
                report.repos_registered += 1;
                if action == ImportedRepoAction::Cloned {
                    report.repos_cloned += 1;
                }
            }
            None => {
                report.repos_skipped += 1;
                report
                    .warnings
                    .push(format!("Skipped repo '{}'", backup_repo.name));
            }
        }
    }

    import_tags(pool, &backup.tags, &mut report).await?;
    let project_id_map =
        import_projects(pool, &backup.projects, &repo_id_map, &mut report).await?;
    import_preferences(pool, &backup.preferences, &repo_id_map, &project_id_map).await?;

    Ok(report)
}

async fn import_repo(
    pool: &SqlitePool,
    git: &GitService,
    repo_root: &Path,
    backup_repo: &BackupRepo,
) -> Result<Option<(Uuid, ImportedRepoAction)>> {
    let destination = repo_root.join(&backup_repo.name);
    let clone_url = backup_repo.clone.url.as_deref();
    let (path, action) = match decide_repo_import_path(
        &backup_repo.path_hint,
        &destination,
        clone_url,
        git,
    ) {
        RepoImportPathDecision::UseExistingHint => {
            (backup_repo.path_hint.clone(), ImportedRepoAction::Registered)
        }
        RepoImportPathDecision::UseExistingDestination => {
            (destination, ImportedRepoAction::Registered)
        }
        RepoImportPathDecision::Clone => {
            let clone_url = clone_url.ok_or_else(|| anyhow!("clone URL missing"))?;
            clone_repo(clone_url, &destination)?;
            (destination, ImportedRepoAction::Cloned)
        }
        RepoImportPathDecision::SkipExistingNonRepoDestination
        | RepoImportPathDecision::SkipNoCloneUrl => return Ok(None),
    };

    let repo = Repo::find_or_create(pool, &path, &backup_repo.display_name).await?;
    let updated = Repo::update(
        pool,
        repo.id,
        &UpdateRepo {
            display_name: Some(Some(backup_repo.display_name.clone())),
            setup_script: Some(backup_repo.config.setup_script.clone()),
            cleanup_script: Some(backup_repo.config.cleanup_script.clone()),
            archive_script: Some(backup_repo.config.archive_script.clone()),
            copy_files: Some(backup_repo.config.copy_files.clone()),
            parallel_setup_script: Some(Some(backup_repo.config.parallel_setup_script)),
            dev_server_script: Some(backup_repo.config.dev_server_script.clone()),
            default_target_branch: Some(backup_repo.config.default_target_branch.clone()),
            default_working_dir: Some(backup_repo.config.default_working_dir.clone()),
        },
    )
    .await?;

    Ok(Some((updated.id, action)))
}

fn clone_repo(clone_url: &str, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let status = Command::new("git")
        .arg("clone")
        .arg(clone_url)
        .arg(destination)
        .status()
        .with_context(|| format!("failed to spawn git clone for {clone_url}"))?;
    if !status.success() {
        bail!("git clone failed for {clone_url} into {}", destination.display());
    }
    Ok(())
}

async fn import_tags(
    pool: &SqlitePool,
    tags: &[BackupTag],
    report: &mut ImportReport,
) -> Result<()> {
    for tag in tags {
        sqlx::query("DELETE FROM tags WHERE tag_name = ?")
            .bind(&tag.tag_name)
            .execute(pool)
            .await?;
        Tag::create(
            pool,
            &CreateTag {
                tag_name: tag.tag_name.clone(),
                content: tag.content.clone(),
            },
        )
        .await?;
        report.tags_imported += 1;
    }
    Ok(())
}

async fn import_projects(
    pool: &SqlitePool,
    projects: &[BackupProject],
    repo_id_map: &HashMap<Uuid, Uuid>,
    report: &mut ImportReport,
) -> Result<HashMap<Uuid, Uuid>> {
    let mut project_id_map = HashMap::new();
    for project in projects {
        let existing_id = find_existing_project_id(pool, project).await?;
        let project_id = existing_id.unwrap_or_else(Uuid::new_v4);
        sqlx::query(
            r#"INSERT INTO projects (id, name, remote_project_id, default_agent_working_dir)
               VALUES (?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name,
                   remote_project_id = excluded.remote_project_id,
                   default_agent_working_dir = excluded.default_agent_working_dir,
                   updated_at = datetime('now', 'subsec')"#,
        )
        .bind(project_id)
        .bind(&project.name)
        .bind(project.remote_project_id)
        .bind(project.default_agent_working_dir.clone())
        .execute(pool)
        .await?;

        project_id_map.insert(project.source_id, project_id);
        report.projects_imported += 1;

        let remapped_defaults = project
            .repo_defaults
            .iter()
            .filter_map(|repo_default| {
                repo_id_map
                    .get(&repo_default.repo_source_id)
                    .map(|repo_id| db::models::scratch::DraftWorkspaceRepo {
                        repo_id: *repo_id,
                        target_branch: repo_default.target_branch.clone(),
                    })
            })
            .collect::<Vec<_>>();

        Scratch::update(
            pool,
            project_id,
            &ScratchType::ProjectRepoDefaults,
            &UpdateScratch {
                payload: ScratchPayload::ProjectRepoDefaults(ProjectRepoDefaultsData {
                    repos: remapped_defaults,
                }),
            },
        )
        .await?;
    }
    Ok(project_id_map)
}

async fn find_existing_project_id(
    pool: &SqlitePool,
    project: &BackupProject,
) -> Result<Option<Uuid>> {
    if let Some(remote_project_id) = project.remote_project_id {
        let row = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM projects WHERE remote_project_id = ? LIMIT 1",
        )
        .bind(remote_project_id)
        .fetch_optional(pool)
        .await?;
        if let Some(id) = row {
            return Ok(Some(id));
        }
    }

    let row = sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects WHERE name = ? LIMIT 1")
        .bind(&project.name)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

async fn import_preferences(
    pool: &SqlitePool,
    preferences: &BackupPreferences,
    repo_id_map: &HashMap<Uuid, Uuid>,
    project_id_map: &HashMap<Uuid, Uuid>,
) -> Result<()> {
    let Some(mut ui_preferences) = preferences.ui_preferences.clone() else {
        return Ok(());
    };

    ui_preferences.file_search_repo_id = ui_preferences
        .file_search_repo_id
        .and_then(|id| remap_string_uuid(&id, repo_id_map));
    ui_preferences.repo_actions =
        remap_string_keyed_map(std::mem::take(&mut ui_preferences.repo_actions), repo_id_map);
    ui_preferences.selected_project_id = ui_preferences
        .selected_project_id
        .and_then(|id| remap_string_uuid(&id, project_id_map));
    ui_preferences.workspace_filters.project_ids = ui_preferences
        .workspace_filters
        .project_ids
        .drain(..)
        .filter_map(|id| remap_string_uuid(&id, project_id_map))
        .collect();
    ui_preferences.kanban_project_view_selections = remap_string_keyed_map(
        std::mem::take(&mut ui_preferences.kanban_project_view_selections),
        project_id_map,
    );
    ui_preferences.kanban_project_view_preferences = remap_string_keyed_map(
        std::mem::take(&mut ui_preferences.kanban_project_view_preferences),
        project_id_map,
    );
    ui_preferences.workspace_panel_states.clear();
    ui_preferences.collapsed_paths.clear();

    Scratch::update(
        pool,
        UI_PREFERENCES_ID,
        &ScratchType::UiPreferences,
        &UpdateScratch {
            payload: ScratchPayload::UiPreferences(ui_preferences),
        },
    )
    .await?;
    Ok(())
}

fn remap_string_uuid(id: &str, map: &HashMap<Uuid, Uuid>) -> Option<String> {
    let old_id = Uuid::parse_str(id).ok()?;
    map.get(&old_id).map(Uuid::to_string)
}

fn remap_string_keyed_map<T>(
    input: HashMap<String, T>,
    id_map: &HashMap<Uuid, Uuid>,
) -> HashMap<String, T> {
    input
        .into_iter()
        .filter_map(|(key, value)| remap_string_uuid(&key, id_map).map(|new_key| (new_key, value)))
        .collect()
}

pub async fn backup_to_file(
    pool: &SqlitePool,
    paths: &BackupPaths,
    output_path: &Path,
) -> Result<()> {
    let backup = export_backup(pool, paths).await?;
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, serde_json::to_string_pretty(&backup)? + "\n")?;
    Ok(())
}

pub async fn import_from_file(
    pool: &SqlitePool,
    paths: &BackupPaths,
    input_path: &Path,
) -> Result<ImportReport> {
    let raw = std::fs::read_to_string(input_path)
        .with_context(|| format!("failed to read backup {}", input_path.display()))?;
    let backup = serde_json::from_str::<VibeKanbanBackupV1>(&raw)
        .with_context(|| format!("failed to parse backup {}", input_path.display()))?;
    import_backup(pool, paths, &backup).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn sanitize_remote_url_strips_url_credentials() {
        assert_eq!(
            sanitize_remote_url("https://user:secret@example.com/org/repo.git"),
            "https://example.com/org/repo.git"
        );
        assert_eq!(
            sanitize_remote_url("https://token@example.com/org/repo.git"),
            "https://example.com/org/repo.git"
        );
    }

    #[test]
    fn sanitize_remote_url_keeps_ssh_style_remote() {
        assert_eq!(
            sanitize_remote_url("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
    }

    #[test]
    fn redact_sensitive_json_omits_env_and_redacts_secret_like_keys() {
        let redacted = redact_sensitive_json(serde_json::json!({
            "executors": {
                "CLAUDE_CODE": {
                    "DEFAULT": {
                        "cmd": {
                            "env": {
                                "ANTHROPIC_API_KEY": "secret"
                            },
                            "token": "abc123",
                            "safe": "kept"
                        }
                    }
                }
            }
        }));

        assert_eq!(
            redacted["executors"]["CLAUDE_CODE"]["DEFAULT"]["cmd"]["safe"],
            "kept"
        );
        assert_eq!(
            redacted["executors"]["CLAUDE_CODE"]["DEFAULT"]["cmd"]["token"],
            "__REDACTED__"
        );
        assert!(
            redacted["executors"]["CLAUDE_CODE"]["DEFAULT"]["cmd"]
                .get("env")
                .is_none()
        );
    }

    #[test]
    fn portable_config_omits_github_tokens_but_keeps_profile_fields() {
        let mut config = Config::default();
        config.github.pat = Some("pat-secret".to_string());
        config.github.oauth_token = Some("oauth-secret".to_string());
        config.github.username = Some("octocat".to_string());

        let portable = portable_config(config);

        assert_eq!(portable.github.pat, None);
        assert_eq!(portable.github.oauth_token, None);
        assert_eq!(portable.github.username.as_deref(), Some("octocat"));
    }

    #[test]
    fn import_decision_skips_existing_non_repo_destination() {
        let temp = TempDir::new().unwrap();
        let destination = temp.path().join("repo");
        fs::create_dir_all(&destination).unwrap();
        let missing_hint = temp.path().join("missing");
        let git = GitService::new();

        assert_eq!(
            decide_repo_import_path(
                &missing_hint,
                &destination,
                Some("https://example.com/org/repo.git"),
                &git,
            ),
            RepoImportPathDecision::SkipExistingNonRepoDestination
        );
    }

    #[test]
    fn import_decision_clones_only_when_destination_is_missing_and_url_exists() {
        let temp = TempDir::new().unwrap();
        let destination = temp.path().join("repo");
        let missing_hint = temp.path().join("missing");
        let git = GitService::new();

        assert_eq!(
            decide_repo_import_path(
                &missing_hint,
                &destination,
                Some("https://example.com/org/repo.git"),
                &git,
            ),
            RepoImportPathDecision::Clone
        );
    }

    #[test]
    fn remap_string_uuid_returns_new_id_and_drops_unknown_values() {
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();
        let map = HashMap::from([(old_id, new_id)]);

        assert_eq!(
            remap_string_uuid(&old_id.to_string(), &map),
            Some(new_id.to_string())
        );
        assert_eq!(remap_string_uuid(&Uuid::new_v4().to_string(), &map), None);
        assert_eq!(remap_string_uuid("not-a-uuid", &map), None);
    }

    #[test]
    fn remap_string_keyed_map_remaps_known_uuid_keys_and_drops_unknown_keys() {
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();
        let id_map = HashMap::from([(old_id, new_id)]);
        let input = HashMap::from([
            (old_id.to_string(), "keep".to_string()),
            (Uuid::new_v4().to_string(), "drop".to_string()),
            ("not-a-uuid".to_string(), "drop".to_string()),
        ]);

        let remapped = remap_string_keyed_map(input, &id_map);

        assert_eq!(remapped.len(), 1);
        assert_eq!(remapped.get(&new_id.to_string()), Some(&"keep".to_string()));
    }
}
