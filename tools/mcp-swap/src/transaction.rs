//! All-selected configuration use and revert transactions.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::catalog::{Client, Paths, known_clients};
use crate::config::{Action, Scope, ServerSpec, read_server, set_server};
use crate::fs::{
    ArtifactClaim, ConfigRoute, FileIdentity, FileSnapshot, FsError, reject_aliases, remove_exact,
    rename_no_replace, resolve_config_route, stable_snapshot, stage_file,
};
use crate::lock::{TransactionLock, ensure_private_directory};
use crate::recovery::{
    Ledger, RecoveryEntry, STATE_MAX_BYTES, ledger_bytes, load_ledger, state_key,
};

const CONFIG_MAX_BYTES: usize = 16 * 1024 * 1024;
const RETIRED_SAFETY: &str = "LIBTMUX_SAFETY";
const TOOLSETS: &str = "LIBTMUX_TOOLSETS";

/// Inputs shared by every client in one `use` transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UseRequest {
    /// Absolute repository root used by Claude project scope.
    pub repo: PathBuf,
    /// MCP server key to replace.
    pub server: String,
    /// Claude layer; non-Claude clients normalize this to user scope.
    pub scope: Scope,
    /// Portable stdio server definition.
    pub spec: ServerSpec,
}

/// Filters applied to one `revert` transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevertRequest {
    /// Optional Claude scope. Non-Claude entries are always user-scoped.
    pub scope: Option<Scope>,
}

/// One configuration layer changed by a transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    /// Canonical client name.
    pub client: String,
    /// Effective configuration layer.
    pub scope: Scope,
    /// Semantic configuration edit.
    pub action: Action,
}

struct UsePlan<'a> {
    client: &'a Client,
    scope: Scope,
    key: String,
    server: String,
    spec: ServerSpec,
    route: ConfigRoute,
    bytes: Vec<u8>,
    action: Action,
    sequence: u64,
    backup_path: PathBuf,
    existing: Option<RecoveryEntry>,
    backup_rewrites: Vec<BackupRewrite>,
}

struct BackupRewrite {
    key: String,
    entry: RecoveryEntry,
    backup: FileSnapshot,
    bytes: Vec<u8>,
}

struct RevertPlan<'a> {
    client: &'a Client,
    entry: RecoveryEntry,
    route: ConfigRoute,
    backup: FileSnapshot,
}

struct RestoredContent {
    mode: u32,
    size: u64,
    sha256: String,
}

impl RestoredContent {
    fn from_backup(entry: &RecoveryEntry, backup: &FileSnapshot) -> Self {
        Self {
            mode: entry.original_mode,
            size: backup.identity.size,
            sha256: backup.identity.sha256.clone(),
        }
    }

    fn matches(&self, identity: &FileIdentity) -> bool {
        self.mode == identity.mode && self.size == identity.size && self.sha256 == identity.sha256
    }
}

struct FileOperation {
    label: String,
    destination: PathBuf,
    stage: Option<PathBuf>,
    staged: Option<FileIdentity>,
    prior: Option<FileSnapshot>,
    recovery: Option<PathBuf>,
    prior_moved: bool,
    new_published: bool,
    limit: usize,
    route: Option<ConfigRoute>,
}

impl FileOperation {
    fn replacement(
        label: impl Into<String>,
        destination: PathBuf,
        stage: PathBuf,
        prior: Option<FileSnapshot>,
        limit: usize,
    ) -> Result<Self, FsError> {
        let staged = stable_snapshot(&stage, limit)?.identity;
        Ok(Self {
            label: label.into(),
            destination,
            stage: Some(stage),
            staged: Some(staged),
            prior,
            recovery: None,
            prior_moved: false,
            new_published: false,
            limit,
            route: None,
        })
    }

    fn deletion(
        label: impl Into<String>,
        destination: PathBuf,
        prior: FileSnapshot,
        limit: usize,
    ) -> Self {
        Self {
            label: label.into(),
            destination,
            stage: None,
            staged: None,
            prior: Some(prior),
            recovery: None,
            prior_moved: false,
            new_published: false,
            limit,
            route: None,
        }
    }

    fn with_route(mut self, route: &ConfigRoute) -> Self {
        self.route = Some(route.clone());
        self
    }

    fn commit(
        &mut self,
        lock: &TransactionLock,
        hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
    ) -> Result<(), FsError> {
        lock.verify()?;
        if let Some(prior) = &self.prior {
            hook(&format!("before-{}-take", self.label))?;
            lock.verify()?;
            if let Some(route) = &self.route {
                route.verify()?;
            }
            let current = stable_snapshot(&self.destination, self.limit)?;
            if current.identity != prior.identity {
                return Err(FsError::new(format!(
                    "{} changed before take-aside",
                    self.label
                )));
            }
            let recovery = recovery_path(&self.destination, &self.label);
            rename_no_replace(&self.destination, &recovery)?;
            self.prior_moved = true;
            self.recovery = Some(recovery.clone());
            let moved = stable_snapshot(&recovery, self.limit)?;
            if moved.identity != prior.identity {
                return Err(FsError::new(format!(
                    "{} changed during take-aside; retained {}",
                    self.label,
                    recovery.display()
                )));
            }
            if let Some(route) = &self.route {
                route.verify_target_absent()?;
            }
        }
        let Some(stage) = self.stage.as_ref() else {
            return Ok(());
        };
        hook(&format!("before-{}-publish", self.label))?;
        lock.verify()?;
        if let Some(route) = &self.route {
            route.verify_target_absent()?;
        }
        rename_no_replace(stage, &self.destination)?;
        self.new_published = true;
        let published = stable_snapshot(&self.destination, self.limit)?;
        if Some(&published.identity) != self.staged.as_ref() {
            return Err(FsError::new(format!(
                "{} changed during publication",
                self.label
            )));
        }
        if let Some(route) = &self.route {
            route.verify_topology()?;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<(), FsError> {
        let mut displaced_new = None;
        if self.new_published {
            let expected = self
                .staged
                .as_ref()
                .ok_or_else(|| FsError::new("published operation has no staged identity"))?;
            let current = stable_snapshot(&self.destination, self.limit)?;
            if current.identity != *expected {
                return Err(FsError::new(format!(
                    "{} changed before rollback; retained recovery artifacts",
                    self.label
                )));
            }
            if let Some(route) = &self.route {
                route.verify_topology()?;
            }
            let path = recovery_path(&self.destination, &format!("{}-new", self.label));
            rename_no_replace(&self.destination, &path)?;
            let moved = stable_snapshot(&path, self.limit)?;
            if moved.identity != *expected {
                return Err(FsError::new(format!(
                    "{} changed during rollback; retained {}",
                    self.label,
                    path.display()
                )));
            }
            displaced_new = Some((path, expected.clone()));
            self.new_published = false;
            if let Some(route) = &self.route {
                route.verify_target_absent()?;
            }
        }
        if self.prior_moved {
            let recovery = self
                .recovery
                .as_ref()
                .ok_or_else(|| FsError::new("moved operation has no recovery path"))?;
            let prior = self
                .prior
                .as_ref()
                .ok_or_else(|| FsError::new("moved operation has no prior snapshot"))?;
            let retained = stable_snapshot(recovery, self.limit)?;
            if retained.identity != prior.identity {
                return Err(FsError::new(format!(
                    "{} recovery changed; retained {}",
                    self.label,
                    recovery.display()
                )));
            }
            rename_no_replace(recovery, &self.destination)?;
            let restored = stable_snapshot(&self.destination, self.limit)?;
            if restored.identity != prior.identity {
                return Err(FsError::new(format!(
                    "{} rollback changed identity",
                    self.label
                )));
            }
            if let Some(route) = &self.route {
                route.verify_topology()?;
            }
            self.prior_moved = false;
        }
        if let Some((path, identity)) = displaced_new {
            remove_exact(&path, &identity, self.limit)?;
        }
        Ok(())
    }

    fn verify_committed(
        &self,
        lock: &TransactionLock,
        hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
        verify_destination: bool,
    ) -> Result<(), FsError> {
        lock.verify()?;
        hook(&format!("before-{}-finish", self.label))?;
        lock.verify()?;
        if self.new_published && verify_destination {
            let expected = self
                .staged
                .as_ref()
                .ok_or_else(|| FsError::new("published operation has no staged identity"))?;
            let current = stable_snapshot(&self.destination, self.limit)?;
            if current.identity != *expected {
                return Err(FsError::new(format!(
                    "{} changed after publication; retained recovery artifacts",
                    self.label
                )));
            }
            if let Some(route) = &self.route {
                route.verify_topology()?;
            }
        } else if !self.new_published && self.prior_moved {
            if let Some(route) = &self.route {
                route.verify_target_absent()?;
            } else {
                match fs::symlink_metadata(&self.destination) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => {
                        return Err(FsError::new(format!(
                            "{} destination appeared; retained recovery artifacts",
                            self.label
                        )));
                    }
                    Err(error) => {
                        return Err(FsError::new(format!(
                            "inspect {} destination: {error}",
                            self.label
                        )));
                    }
                }
            }
        }
        if self.prior_moved {
            let recovery = self
                .recovery
                .as_ref()
                .ok_or_else(|| FsError::new("moved operation has no recovery path"))?;
            let expected = &self
                .prior
                .as_ref()
                .ok_or_else(|| FsError::new("moved operation has no prior snapshot"))?
                .identity;
            if stable_snapshot(recovery, self.limit)?.identity != *expected {
                return Err(FsError::new(format!(
                    "{} recovery changed before cleanup",
                    self.label
                )));
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), FsError> {
        if self.prior_moved {
            let path = self
                .recovery
                .as_ref()
                .ok_or_else(|| FsError::new("moved operation has no recovery path"))?;
            let identity = &self
                .prior
                .as_ref()
                .ok_or_else(|| FsError::new("moved operation has no prior snapshot"))?
                .identity;
            remove_exact(path, identity, self.limit)?;
            self.prior_moved = false;
        }
        Ok(())
    }

    fn cleanup_stage(&self) -> Result<(), FsError> {
        let Some(stage) = &self.stage else {
            return Ok(());
        };
        match fs::symlink_metadata(stage) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(_) => {}
            Err(error) => {
                return Err(FsError::new(format!(
                    "inspect staged {}: {error}",
                    self.label
                )));
            }
        }
        let identity = self
            .staged
            .as_ref()
            .ok_or_else(|| FsError::new("stage has no authenticated identity"))?;
        remove_exact(stage, identity, self.limit)
    }
}

/// Apply one server definition to every selected client as one transaction.
///
/// # Errors
///
/// Returns [`FsError`] when planning, authentication, staging, publication,
/// or exact rollback fails.
pub fn use_clients(
    paths: &Paths,
    clients: &[&Client],
    request: &UseRequest,
    dry_run: bool,
) -> Result<Vec<Change>, FsError> {
    use_clients_impl(paths, clients, request, dry_run, None, &mut |_| Ok(()))
}

/// Resolve the exact per-client server definitions that require preflight.
///
/// # Errors
///
/// Returns [`FsError`] when configuration or recovery state cannot be planned
/// and authenticated without writing.
pub fn planned_use_specs(
    paths: &Paths,
    clients: &[&Client],
    request: &UseRequest,
) -> Result<Vec<ServerSpec>, FsError> {
    require_absolute_repo(request)?;
    let ledger = load_optional_ledger(&paths.state_file())?.0;
    let plans = plan_use(clients, request, &ledger)?;
    validate_use_aliases(paths, &plans, &ledger, None)?;
    Ok(specs_from_use(&plans))
}

/// Apply a server definition only if its locked per-client plans match those
/// that were preflighted.
///
/// # Errors
///
/// Returns [`FsError`] when locked replanning differs from the preflighted
/// definitions or when the exact transaction cannot complete.
pub fn use_clients_preflighted(
    paths: &Paths,
    clients: &[&Client],
    request: &UseRequest,
    preflighted: &[ServerSpec],
) -> Result<Vec<Change>, FsError> {
    use_clients_impl(
        paths,
        clients,
        request,
        false,
        Some(preflighted),
        &mut |_| Ok(()),
    )
}

/// Apply one server definition with a test boundary hook.
///
/// # Errors
///
/// Returns [`FsError`] under the same conditions as [`use_clients`], including
/// an injected boundary failure.
pub fn use_clients_with_hook(
    paths: &Paths,
    clients: &[&Client],
    request: &UseRequest,
    dry_run: bool,
    hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
) -> Result<Vec<Change>, FsError> {
    use_clients_impl(paths, clients, request, dry_run, None, hook)
}

fn use_clients_impl(
    paths: &Paths,
    clients: &[&Client],
    request: &UseRequest,
    dry_run: bool,
    preflighted: Option<&[ServerSpec]>,
    hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
) -> Result<Vec<Change>, FsError> {
    require_absolute_repo(request)?;
    if dry_run {
        let ledger = load_optional_ledger(&paths.state_file())?.0;
        let plans = plan_use(clients, request, &ledger)?;
        validate_use_aliases(paths, &plans, &ledger, None)?;
        return Ok(changes_from_use(&plans));
    }
    let lock = TransactionLock::acquire(&paths.lock_dir())?;
    let (ledger, state_snapshot) = load_optional_ledger(&paths.state_file())?;
    let plans = plan_use(clients, request, &ledger)?;
    validate_use_aliases(paths, &plans, &ledger, Some(&lock))?;
    if preflighted.is_some_and(|expected| expected != specs_from_use(&plans)) {
        return Err(FsError::new(
            "configuration changed after preflight; retry the swap",
        ));
    }
    let changes = changes_from_use(&plans);
    if plans.iter().all(|plan| plan.action == Action::Unchanged) {
        return Ok(changes);
    }
    ensure_private_directory(&paths.state_dir())?;

    let (mut operations, config_operation_start) =
        stage_use(paths, &plans, &ledger, state_snapshot.as_ref())?;
    let result = commit_use(&lock, &plans, &mut operations, config_operation_start, hook);
    settle_operations(&lock, &mut operations, result, hook)?;
    Ok(changes)
}

fn require_absolute_repo(request: &UseRequest) -> Result<(), FsError> {
    if request.repo.is_absolute() {
        Ok(())
    } else {
        Err(FsError::new("repository path must be absolute"))
    }
}

/// Restore every selected outstanding layer as one transaction.
///
/// # Errors
///
/// Returns [`FsError`] when any selected record, config, backup, state, or
/// rollback boundary cannot be authenticated.
pub fn revert_clients(
    paths: &Paths,
    clients: &[&Client],
    request: RevertRequest,
    dry_run: bool,
) -> Result<Vec<Change>, FsError> {
    revert_clients_with_hook(paths, clients, request, dry_run, &mut |_| Ok(()))
}

/// Restore selected layers with a test boundary hook.
///
/// # Errors
///
/// Returns [`FsError`] under the same conditions as [`revert_clients`].
pub fn revert_clients_with_hook(
    paths: &Paths,
    clients: &[&Client],
    request: RevertRequest,
    dry_run: bool,
    hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
) -> Result<Vec<Change>, FsError> {
    if dry_run {
        let ledger = load_optional_ledger(&paths.state_file())?.0;
        let plans = plan_revert(clients, request, &ledger)?;
        validate_revert_aliases(paths, &plans, &ledger, None)?;
        return Ok(changes_from_revert(&plans));
    }
    let lock = TransactionLock::acquire(&paths.lock_dir())?;
    let (ledger, state_snapshot) = load_optional_ledger(&paths.state_file())?;
    let state_snapshot =
        state_snapshot.ok_or_else(|| FsError::new("no recovery state to revert"))?;
    let plans = plan_revert(clients, request, &ledger)?;
    validate_revert_aliases(paths, &plans, &ledger, Some(&lock))?;
    let changes = changes_from_revert(&plans);
    if plans.is_empty() {
        return Ok(changes);
    }
    ensure_private_directory(&paths.state_dir())?;
    let (mut operations, state_index, backup_start) =
        stage_revert(paths, &plans, &ledger, &state_snapshot)?;
    let result = commit_revert(&lock, &mut operations, state_index, backup_start, hook);
    settle_operations(&lock, &mut operations, result, hook)?;
    Ok(changes)
}

fn effective_scope(client: &Client, scope: Scope) -> Scope {
    if client.name.as_str() == "claude" {
        scope
    } else {
        Scope::User
    }
}

fn migrate_environment(
    mut existing: BTreeMap<String, String>,
    requested: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, FsError> {
    if requested.contains_key(RETIRED_SAFETY) {
        return Err(FsError::new(
            "LIBTMUX_SAFETY is retired; use LIBTMUX_TOOLSETS",
        ));
    }
    if requested.contains_key(TOOLSETS) {
        existing.remove(RETIRED_SAFETY);
    }
    existing.extend(requested.clone());
    Ok(existing)
}

fn plan_use<'a>(
    clients: &'a [&Client],
    request: &UseRequest,
    ledger: &Ledger,
) -> Result<Vec<UsePlan<'a>>, FsError> {
    let mut plans = Vec::new();
    let mut next_sequence = ledger.next_sequence;
    for client in clients {
        let scope = effective_scope(client, request.scope);
        let key = state_key(client.name.as_str(), scope);
        let route = resolve_config_route(&client.config_path, CONFIG_MAX_BYTES)
            .map_err(|error| FsError::new(format!("{}: {error}", client.name.as_str())))?;
        let mut spec = request.spec.clone();
        let existing_environment = read_server(
            &client.config,
            &route.file.bytes,
            &request.server,
            &request.repo,
            scope,
        )
        .map_err(|error| FsError::new(format!("{}: {error}", client.name.as_str())))?
        .map_or_else(BTreeMap::new, |existing| existing.env);
        spec.env = migrate_environment(existing_environment, &spec.env)?;
        let edit = set_server(
            &client.config,
            &route.file.bytes,
            &request.server,
            &spec,
            &request.repo,
            scope,
        )
        .map_err(|error| FsError::new(format!("{}: {error}", client.name.as_str())))?;
        let existing = ledger.entries.get(&key).cloned();
        let sequence = existing.as_ref().map_or_else(
            || {
                let sequence = next_sequence;
                next_sequence += 1;
                sequence
            },
            |entry| entry.sequence,
        );
        let backup_path = existing.as_ref().map_or_else(
            || backup_path(&route.target, sequence),
            |entry| entry.backup_path.clone(),
        );
        let backup_rewrites = if let Some(entry) = &existing {
            plan_recovery_rewrites(client, scope, request, &spec, ledger, entry, &route)?
        } else if fs::symlink_metadata(&backup_path).is_ok() {
            return Err(FsError::new(format!(
                "backup destination already exists: {}",
                backup_path.display()
            )));
        } else {
            Vec::new()
        };
        plans.push(UsePlan {
            client,
            scope,
            key,
            server: request.server.clone(),
            spec,
            route,
            bytes: edit.bytes,
            action: edit.action,
            sequence,
            backup_path,
            existing,
            backup_rewrites,
        });
    }
    Ok(plans)
}

fn plan_recovery_rewrites(
    client: &Client,
    scope: Scope,
    request: &UseRequest,
    spec: &ServerSpec,
    ledger: &Ledger,
    entry: &RecoveryEntry,
    route: &ConfigRoute,
) -> Result<Vec<BackupRewrite>, FsError> {
    verify_entry_route(entry, route)?;
    let backup = stable_snapshot(&entry.backup_path, CONFIG_MAX_BYTES)?;
    if backup.identity != entry.backup {
        return Err(FsError::new(format!(
            "{} backup changed",
            client.name.as_str()
        )));
    }
    let mut newer: Vec<_> = ledger
        .entries
        .iter()
        .filter(|(_, candidate)| {
            candidate.target_path == entry.target_path && candidate.sequence > entry.sequence
        })
        .collect();
    newer.sort_by_key(|(_, candidate)| candidate.sequence);
    let expected = newer.last().map_or(&entry.expected_config, |(_, newest)| {
        &newest.expected_config
    });
    if route.file.identity != *expected {
        return Err(FsError::new(format!(
            "{} config changed since mcp-swap wrote its newest layer",
            client.name.as_str()
        )));
    }
    newer
        .into_iter()
        .map(|(key, newer_entry)| {
            verify_entry_route(newer_entry, route)?;
            let newer_backup = stable_snapshot(&newer_entry.backup_path, CONFIG_MAX_BYTES)?;
            if newer_backup.identity != newer_entry.backup {
                return Err(FsError::new(format!(
                    "{} backup changed",
                    newer_entry.client
                )));
            }
            let edit = set_server(
                &client.config,
                &newer_backup.bytes,
                &request.server,
                spec,
                &request.repo,
                scope,
            )
            .map_err(|error| {
                FsError::new(format!("{} recovery chain: {error}", client.name.as_str()))
            })?;
            Ok(BackupRewrite {
                key: key.clone(),
                entry: newer_entry.clone(),
                backup: newer_backup,
                bytes: edit.bytes,
            })
        })
        .collect()
}

fn plan_revert<'a>(
    clients: &'a [&Client],
    request: RevertRequest,
    ledger: &Ledger,
) -> Result<Vec<RevertPlan<'a>>, FsError> {
    let names: BTreeSet<_> = clients.iter().map(|client| client.name.as_str()).collect();
    let mut plans = Vec::new();
    let mut entries: Vec<_> = ledger
        .entries
        .values()
        .filter(|entry| names.contains(entry.client.as_str()))
        .filter(|entry| {
            entry.client != "claude" || request.scope.is_none_or(|scope| scope == entry.scope)
        })
        .collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.sequence));
    let mut restored = BTreeMap::<PathBuf, RestoredContent>::new();
    for entry in entries {
        let client = clients
            .iter()
            .copied()
            .find(|client| client.name.as_str() == entry.client)
            .ok_or_else(|| FsError::new("recovery client is not selected"))?;
        let route = resolve_config_route(&client.config_path, CONFIG_MAX_BYTES)?;
        verify_entry_route(entry, &route)?;
        let matches = restored.get(&entry.target_path).map_or_else(
            || route.file.identity == entry.expected_config,
            |content| content.matches(&entry.expected_config),
        );
        if !matches {
            return Err(FsError::new(format!(
                "{} config changed before revert",
                entry.client
            )));
        }
        let backup = stable_snapshot(&entry.backup_path, CONFIG_MAX_BYTES)?;
        if backup.identity != entry.backup {
            return Err(FsError::new(format!(
                "{} backup changed before revert",
                entry.client
            )));
        }
        restored.insert(
            entry.target_path.clone(),
            RestoredContent::from_backup(entry, &backup),
        );
        plans.push(RevertPlan {
            client,
            entry: entry.clone(),
            route,
            backup,
        });
    }
    Ok(plans)
}

fn verify_entry_route(entry: &RecoveryEntry, route: &ConfigRoute) -> Result<(), FsError> {
    if entry.config_path != route.logical
        || entry.target_path != route.target
        || entry.route_links != route.links
        || entry.route_anchors != route.anchors
        || entry.route_parent != route.parent
    {
        return Err(FsError::new(format!(
            "{} configuration route changed",
            entry.client
        )));
    }
    Ok(())
}

fn validate_use_aliases(
    paths: &Paths,
    plans: &[UsePlan<'_>],
    ledger: &Ledger,
    lock: Option<&TransactionLock>,
) -> Result<(), FsError> {
    let mut claims = all_config_claims(paths, |name| {
        plans
            .iter()
            .find(|plan| plan.client.name.as_str() == name)
            .map(|plan| &plan.route)
    })?;
    add_recovery_claims(&mut claims, ledger)?;
    for plan in plans {
        if plan.existing.is_none() {
            claims.push(ArtifactClaim::for_path(
                format!("{} backup", plan.client.name.as_str()),
                &plan.backup_path,
            )?);
        }
    }
    claims.push(ArtifactClaim::for_path(
        "recovery state",
        &paths.state_file(),
    )?);
    if let Some(lock) = lock {
        claims.push(ArtifactClaim::for_path("state lock", lock.path())?);
    } else {
        claims.push(ArtifactClaim::for_path("state lock", &paths.lock_file())?);
    }
    reject_aliases(&claims)
}

fn validate_revert_aliases(
    paths: &Paths,
    plans: &[RevertPlan<'_>],
    ledger: &Ledger,
    lock: Option<&TransactionLock>,
) -> Result<(), FsError> {
    let mut claims = all_config_claims(paths, |name| {
        plans
            .iter()
            .find(|plan| plan.client.name.as_str() == name)
            .map(|plan| &plan.route)
    })?;
    add_recovery_claims(&mut claims, ledger)?;
    claims.push(ArtifactClaim::for_path(
        "recovery state",
        &paths.state_file(),
    )?);
    if let Some(lock) = lock {
        claims.push(ArtifactClaim::for_path("state lock", lock.path())?);
    } else {
        claims.push(ArtifactClaim::for_path("state lock", &paths.lock_file())?);
    }
    reject_aliases(&claims)
}

fn all_config_claims<'a>(
    paths: &Paths,
    selected_route: impl Fn(&str) -> Option<&'a ConfigRoute>,
) -> Result<Vec<ArtifactClaim>, FsError> {
    known_clients(paths)
        .into_iter()
        .map(|client| {
            let label = format!("{} config", client.name.as_str());
            if let Some(route) = selected_route(client.name.as_str()) {
                return Ok(ArtifactClaim::from_route(label, route));
            }
            match fs::symlink_metadata(&client.config_path) {
                Ok(_) => resolve_config_route(&client.config_path, CONFIG_MAX_BYTES)
                    .map(|route| ArtifactClaim::from_route(label, &route)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ArtifactClaim::for_path(label, &client.config_path)
                }
                Err(error) => Err(FsError::new(format!(
                    "inspect {} config: {error}",
                    client.name.as_str()
                ))),
            }
        })
        .collect()
}

fn add_recovery_claims(claims: &mut Vec<ArtifactClaim>, ledger: &Ledger) -> Result<(), FsError> {
    for (key, entry) in &ledger.entries {
        claims.push(ArtifactClaim::for_path(
            format!("{key} recovery backup"),
            &entry.backup_path,
        )?);
    }
    Ok(())
}

fn stage_use(
    paths: &Paths,
    plans: &[UsePlan<'_>],
    ledger: &Ledger,
    state_snapshot: Option<&FileSnapshot>,
) -> Result<(Vec<FileOperation>, usize), FsError> {
    let mut operations = Vec::new();
    let mut next = ledger.clone();
    next.next_sequence = plans
        .iter()
        .map(|plan| plan.sequence + 1)
        .chain(std::iter::once(ledger.next_sequence))
        .max()
        .unwrap_or(ledger.next_sequence);
    for plan in plans {
        if plan.action == Action::Unchanged {
            continue;
        }
        stage_use_plan(plan, &mut next, &mut operations)?;
    }
    let mut config_operations = Vec::new();
    let mut prefix = Vec::new();
    for operation in operations {
        if operation.label.starts_with("config-") {
            config_operations.push(operation);
        } else {
            prefix.push(operation);
        }
    }
    let state_stage = stage_file(&paths.state_file(), &ledger_bytes(&next)?, 0o600)?;
    prefix.push(FileOperation::replacement(
        "state",
        paths.state_file(),
        state_stage,
        state_snapshot.cloned(),
        STATE_MAX_BYTES,
    )?);
    let config_start = prefix.len();
    prefix.extend(config_operations);
    Ok((prefix, config_start))
}

fn stage_use_plan(
    plan: &UsePlan<'_>,
    next: &mut Ledger,
    operations: &mut Vec<FileOperation>,
) -> Result<(), FsError> {
    let config_stage = stage_file(
        &plan.route.target,
        &plan.bytes,
        plan.route.file.identity.mode,
    )?;
    let config_identity = stable_snapshot(&config_stage, CONFIG_MAX_BYTES)?.identity;
    let backup_identity = if let Some(entry) = &plan.existing {
        entry.backup.clone()
    } else {
        let backup_stage = stage_file(&plan.backup_path, &plan.route.file.bytes, 0o600)?;
        let identity = stable_snapshot(&backup_stage, CONFIG_MAX_BYTES)?.identity;
        operations.push(FileOperation::replacement(
            format!("backup-{}", plan.client.name.as_str()),
            plan.backup_path.clone(),
            backup_stage,
            None,
            CONFIG_MAX_BYTES,
        )?);
        identity
    };
    stage_recovery_rewrites(plan, next, operations)?;
    let rewritten: Vec<_> = plan
        .backup_rewrites
        .iter()
        .map(|rewrite| {
            let mut identity = next
                .entries
                .get(&rewrite.key)
                .ok_or_else(|| FsError::new("recovery-chain entry disappeared"))?
                .backup
                .clone();
            identity.mode = plan.route.file.identity.mode;
            Ok((rewrite.key.clone(), identity))
        })
        .collect::<Result<_, FsError>>()?;
    for (index, (key, _)) in rewritten.iter().enumerate() {
        let expected = rewritten
            .get(index + 1)
            .map_or_else(|| config_identity.clone(), |(_, identity)| identity.clone());
        next.entries
            .get_mut(key)
            .ok_or_else(|| FsError::new("recovery-chain entry disappeared"))?
            .expected_config = expected;
    }
    let expected_config = rewritten
        .first()
        .map_or(config_identity, |(_, identity)| identity.clone());
    next.entries.insert(
        plan.key.clone(),
        RecoveryEntry {
            client: plan.client.name.as_str().into(),
            scope: plan.scope,
            sequence: plan.sequence,
            server: plan.server.clone(),
            config_path: plan.route.logical.clone(),
            target_path: plan.route.target.clone(),
            route_links: plan.route.links.clone(),
            route_anchors: plan.route.anchors.clone(),
            route_parent: plan.route.parent.clone(),
            original_mode: plan
                .existing
                .as_ref()
                .map_or(plan.route.file.identity.mode, |entry| entry.original_mode),
            backup_path: plan.backup_path.clone(),
            backup: backup_identity,
            expected_config,
        },
    );
    operations.push(
        FileOperation::replacement(
            format!("config-{}", plan.client.name.as_str()),
            plan.route.target.clone(),
            config_stage,
            Some(plan.route.file.clone()),
            CONFIG_MAX_BYTES,
        )?
        .with_route(&plan.route),
    );
    Ok(())
}

fn stage_recovery_rewrites(
    plan: &UsePlan<'_>,
    next: &mut Ledger,
    operations: &mut Vec<FileOperation>,
) -> Result<(), FsError> {
    for rewrite in &plan.backup_rewrites {
        let stage = stage_file(&rewrite.entry.backup_path, &rewrite.bytes, 0o600)?;
        let identity = stable_snapshot(&stage, CONFIG_MAX_BYTES)?.identity;
        next.entries
            .get_mut(&rewrite.key)
            .ok_or_else(|| FsError::new("recovery-chain entry disappeared"))?
            .backup = identity;
        let scope = match rewrite.entry.scope {
            Scope::User => "user",
            Scope::Project => "project",
        };
        operations.push(FileOperation::replacement(
            format!("chain-backup-{}-{scope}", rewrite.entry.client),
            rewrite.entry.backup_path.clone(),
            stage,
            Some(rewrite.backup.clone()),
            CONFIG_MAX_BYTES,
        )?);
    }
    Ok(())
}

fn commit_use(
    lock: &TransactionLock,
    plans: &[UsePlan<'_>],
    operations: &mut [FileOperation],
    config_start: usize,
    hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
) -> Result<(), FsError> {
    for operation in &mut operations[..config_start] {
        operation.commit(lock, hook)?;
    }
    for (operation, plan) in operations[config_start..]
        .iter_mut()
        .zip(plans.iter().filter(|plan| plan.action != Action::Unchanged))
    {
        plan.route.verify()?;
        operation.commit(lock, hook)?;
    }
    Ok(())
}

fn stage_revert(
    paths: &Paths,
    plans: &[RevertPlan<'_>],
    ledger: &Ledger,
    state_snapshot: &FileSnapshot,
) -> Result<(Vec<FileOperation>, usize, usize), FsError> {
    let mut operations = Vec::new();
    let mut restored_identities = Vec::new();
    let mut current = BTreeMap::<PathBuf, FileSnapshot>::new();
    for plan in plans {
        let prior = current
            .get(&plan.route.target)
            .cloned()
            .unwrap_or_else(|| plan.route.file.clone());
        let stage = stage_file(
            &plan.route.target,
            &plan.backup.bytes,
            plan.entry.original_mode,
        )?;
        let restored = stable_snapshot(&stage, CONFIG_MAX_BYTES)?;
        restored_identities.push((
            plan.entry.target_path.clone(),
            plan.entry.sequence,
            restored.identity.clone(),
        ));
        let mut route = plan.route.clone();
        route.file = prior.clone();
        operations.push(
            FileOperation::replacement(
                format!("config-{}", plan.client.name.as_str()),
                plan.route.target.clone(),
                stage,
                Some(prior),
                CONFIG_MAX_BYTES,
            )?
            .with_route(&route),
        );
        current.insert(
            plan.route.target.clone(),
            FileSnapshot {
                path: plan.route.target.clone(),
                identity: restored.identity,
                bytes: restored.bytes,
            },
        );
    }
    let mut next = ledger.clone();
    for plan in plans {
        next.entries
            .remove(&state_key(&plan.entry.client, plan.entry.scope));
    }
    for (target, removed_sequence, restored) in restored_identities {
        if let Some(predecessor) = next
            .entries
            .values_mut()
            .filter(|entry| entry.target_path == target && entry.sequence < removed_sequence)
            .max_by_key(|entry| entry.sequence)
        {
            predecessor.expected_config = restored;
        }
    }
    let state_index = operations.len();
    if next.entries.is_empty() {
        operations.push(FileOperation::deletion(
            "state",
            paths.state_file(),
            state_snapshot.clone(),
            STATE_MAX_BYTES,
        ));
    } else {
        let state_stage = stage_file(&paths.state_file(), &ledger_bytes(&next)?, 0o600)?;
        operations.push(FileOperation::replacement(
            "state",
            paths.state_file(),
            state_stage,
            Some(state_snapshot.clone()),
            STATE_MAX_BYTES,
        )?);
    }
    let backup_start = operations.len();
    for plan in plans {
        operations.push(FileOperation::deletion(
            format!("backup-{}", plan.client.name.as_str()),
            plan.entry.backup_path.clone(),
            plan.backup.clone(),
            CONFIG_MAX_BYTES,
        ));
    }
    Ok((operations, state_index, backup_start))
}

fn commit_revert(
    lock: &TransactionLock,
    operations: &mut [FileOperation],
    state_index: usize,
    backup_start: usize,
    hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
) -> Result<(), FsError> {
    for operation in &mut operations[..state_index] {
        operation.commit(lock, hook)?;
    }
    operations[state_index].commit(lock, hook)?;
    for operation in &mut operations[backup_start..] {
        operation.commit(lock, hook)?;
    }
    Ok(())
}

fn settle_operations(
    lock: &TransactionLock,
    operations: &mut [FileOperation],
    result: Result<(), FsError>,
    hook: &mut dyn FnMut(&str) -> Result<(), FsError>,
) -> Result<(), FsError> {
    match result {
        Ok(()) => {
            let mut later_destinations = BTreeSet::new();
            let mut verify_destinations = vec![false; operations.len()];
            for (index, operation) in operations.iter().enumerate().rev() {
                verify_destinations[index] =
                    later_destinations.insert(operation.destination.clone());
            }
            for (operation, verify_destination) in operations.iter().zip(verify_destinations) {
                if let Err(error) = operation.verify_committed(lock, hook, verify_destination) {
                    return rollback_operations(operations, error);
                }
            }
            for operation in operations.iter_mut() {
                operation.finish()?;
                operation.cleanup_stage()?;
            }
            Ok(())
        }
        Err(error) => rollback_operations(operations, error),
    }
}

fn rollback_operations(operations: &mut [FileOperation], error: FsError) -> Result<(), FsError> {
    let mut rollback = Vec::new();
    for operation in operations.iter_mut().rev() {
        if let Err(rollback_error) = operation.rollback() {
            rollback.push(rollback_error.to_string());
        }
    }
    for operation in operations.iter() {
        if let Err(cleanup_error) = operation.cleanup_stage() {
            rollback.push(cleanup_error.to_string());
        }
    }
    if rollback.is_empty() {
        Err(error)
    } else {
        Err(FsError::new(format!(
            "{error}; rollback incomplete: {}",
            rollback.join("; ")
        )))
    }
}

fn changes_from_use(plans: &[UsePlan<'_>]) -> Vec<Change> {
    plans
        .iter()
        .map(|plan| Change {
            client: plan.client.name.as_str().into(),
            scope: plan.scope,
            action: plan.action,
        })
        .collect()
}

fn specs_from_use(plans: &[UsePlan<'_>]) -> Vec<ServerSpec> {
    plans.iter().map(|plan| plan.spec.clone()).collect()
}

fn changes_from_revert(plans: &[RevertPlan<'_>]) -> Vec<Change> {
    plans
        .iter()
        .map(|plan| Change {
            client: plan.client.name.as_str().into(),
            scope: plan.entry.scope,
            action: Action::Removed,
        })
        .collect()
}

fn load_optional_ledger(path: &Path) -> Result<(Ledger, Option<FileSnapshot>), FsError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let ledger = load_ledger(path)?;
            let snapshot = stable_snapshot(path, STATE_MAX_BYTES)?;
            Ok((ledger, Some(snapshot)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((Ledger::default(), None)),
        Err(error) => Err(FsError::new(format!("inspect recovery state: {error}"))),
    }
}

fn backup_path(target: &Path, sequence: u64) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    target.with_file_name(format!("{name}.bak.mcp-swap-rust-{sequence:020}"))
}

fn recovery_path(destination: &Path, label: &str) -> PathBuf {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    destination.with_file_name(format!(
        ".{name}.mcp-swap-{label}-recovery-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ))
}
