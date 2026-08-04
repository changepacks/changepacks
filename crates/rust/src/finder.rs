use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Package, Project, ProjectFinder};
use std::{
    collections::{HashMap, HashSet},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::{
    package::RustPackage,
    workspace::{InheritedWorkspaceMembers, RustWorkspace},
};

/// Package info deferred for workspace version resolution
#[derive(Debug)]
struct PendingWorkspacePackage {
    name: Option<String>,
    abs_path: PathBuf,
    relative_path: PathBuf,
    dependencies: Vec<String>,
    publishable_by_default: bool,
}

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["Cargo.toml"];

fn cargo_toml_does_not_exist(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::NotFound)
    })
}

/// Parse an ancestor `Cargo.toml` that is only a *candidate*: the walk that
/// produced `candidate` guessed the path, so "no such file" is an ordinary
/// negative answer (`Ok(None)`) rather than a failure. Any other error — a
/// malformed manifest, a permission problem — is still propagated with the
/// path context [`crate::read_and_parse_cargo_toml`] attached.
///
/// Both ancestor walks (`discover_workspace_dependency_aliases_for_member` and
/// `discover_workspace_root_for_member`) need exactly this discrimination, so
/// it has a single definition here next to [`cargo_toml_does_not_exist`]; the
/// two cannot drift into disagreeing about which errors are "missing".
async fn read_cargo_toml_if_absent_ok(candidate: &Path) -> Result<Option<toml_edit::DocumentMut>> {
    match crate::read_and_parse_cargo_toml(candidate).await {
        Ok((_, parsed)) => Ok(Some(parsed)),
        Err(error) if cargo_toml_does_not_exist(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Look up `[package].<field>` as an owned string, mirroring the
/// `doc.get("package").and_then(|p| p.get(field)).and_then(|v| v.as_str()).map(String::from)`
/// chain that used to be open-coded across `visit` and `finalize`. Extracted so
/// a future manifest shape change (e.g. inline-table `name = { workspace = true }`)
/// only needs to be adapted in one place.
///
/// This helper now only encodes WHERE the table lives; the shared read tail is
/// [`changepacks_utils::toml_item_str`], which `changepacks-python`'s
/// `[project]` reader uses too.
///
/// See [`workspace_package_str`] for the `[workspace.package].<field>` sibling.
fn package_str(doc: &toml_edit::DocumentMut, field: &str) -> Option<String> {
    changepacks_utils::toml_item_str(doc.get("package"), field)
}

/// Cargo accepts either a boolean or a registry allow-list for a `publish`
/// value. Publishing is disabled by the exact boolean `false` *and* by an
/// empty allow-list `publish = []` — Cargo refuses `cargo publish` for both,
/// so treating `[]` as publishable would list a crate that always fails to
/// publish. A missing key, `true`, or a non-empty list all stay publishable,
/// as does any other scalar shape Cargo would itself reject.
///
/// Shared by the `[package].publish` decoder ([`package_publish_default`]) and
/// its `[workspace.package].publish` sibling
/// ([`workspace_package_publishable_by_default`]) so the rule lives in one
/// place and inherited answers cannot drift from standalone ones.
fn publish_item_publishable(publish: &toml_edit::Item) -> bool {
    if publish.as_bool() == Some(false) {
        return false;
    }
    if let Some(registries) = publish.as_array() {
        return !registries.is_empty();
    }
    true
}

/// How a manifest answers "is this package publishable by default?".
///
/// `publish` is an inheritable Cargo `[package]` field, so a workspace member
/// may write `publish.workspace = true` and take the value from the workspace
/// root's `[workspace.package].publish`. That shape is table-like, so the
/// scalar rule in [`publish_item_publishable`] cannot answer it on its own and
/// the decision has to be deferred to the workspace root the same way an
/// inherited `version` already is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishDefault {
    /// The manifest answers on its own — no workspace lookup needed.
    Standalone(bool),
    /// `publish.workspace = true`: the answer lives in the workspace root's
    /// `[workspace.package].publish`.
    InheritWorkspace,
}

impl PublishDefault {
    /// Collapse to a plain `bool`, consulting `workspace_answer` only for the
    /// inherit shape. `None` from `workspace_answer` means no workspace root
    /// was found, which Cargo treats as an error but which this finder reports
    /// as publishable — the same permissive default a manifest without a
    /// `publish` key gets.
    fn resolve(self, workspace_answer: impl FnOnce() -> Option<bool>) -> bool {
        match self {
            Self::Standalone(publishable) => publishable,
            Self::InheritWorkspace => workspace_answer().unwrap_or(true),
        }
    }
}

/// Decode `[package].publish`, distinguishing a standalone answer from the
/// `publish.workspace = true` inherit marker.
fn package_publish_default(doc: &toml_edit::DocumentMut) -> PublishDefault {
    let Some(publish) = doc
        .get("package")
        .and_then(|package| package.get("publish"))
    else {
        return PublishDefault::Standalone(true);
    };
    if crate::is_workspace_marker(publish) {
        return PublishDefault::InheritWorkspace;
    }
    PublishDefault::Standalone(publish_item_publishable(publish))
}

/// Locate the `[workspace.package]` table — the single place this file encodes
/// where inheritable Cargo fields live in a workspace root manifest. Every
/// reader of that table goes through here, so a future manifest shape change
/// only has to be adapted once.
fn workspace_package_table(doc: &toml_edit::DocumentMut) -> Option<&toml_edit::Item> {
    doc.get("workspace").and_then(|w| w.get("package"))
}

/// Look up `[workspace.package].<field>` as an owned string. Used twice in this
/// file (once in `visit` to seed `workspace_package_version`, once in
/// `finalize` to walk up to a missed workspace root), matching the
/// `[package].<field>` sibling helper.
fn workspace_package_str(doc: &toml_edit::DocumentMut, field: &str) -> Option<String> {
    changepacks_utils::toml_item_str(workspace_package_table(doc), field)
}

/// Apply the [`publish_item_publishable`] rule to `[workspace.package].publish`
/// — the value a member inheriting via `publish.workspace = true` resolves to.
/// A root that declares nothing is publishable, matching Cargo's own default.
///
/// Sibling of [`workspace_package_str`]; both read the table located by
/// [`workspace_package_table`].
fn workspace_package_publishable_by_default(doc: &toml_edit::DocumentMut) -> bool {
    workspace_package_table(doc)
        .and_then(|p| p.get("publish"))
        .is_none_or(publish_item_publishable)
}

/// Return `true` for a `toml_edit::Item` whose value is table-like with a
/// `path` key — the shape Cargo uses for direct local-path dependencies
/// (`dep = { path = "../dep" }`, optionally alongside `version`). Sibling
/// of [`crate::is_workspace_marker`]: local-path edges are in-repo dependencies
/// just like workspace-inherited ones, so they must feed publish ordering
/// and reverse updates too. Registry dependencies (`dep = "1.0"`) are
/// scalars, so `as_table_like()` returns `None` and they are excluded.
fn is_local_path_dep(value: &toml_edit::Item) -> bool {
    value
        .as_table_like()
        .is_some_and(|table| table.contains_key("path"))
}

/// Return `true` for a dependency entry that carries a registry version
/// requirement — either the scalar shorthand (`dep = "1"`) or a table-like
/// entry with a string `version` (`dep = { path = "../dep", version = "1" }`).
///
/// This is exactly the condition under which Cargo keeps a `dev-dependencies`
/// entry in a packaged manifest: "when a package is published, only
/// dev-dependencies that specify a `version` will be included in the published
/// crate". A path-only dev-dependency is erased at package time, so it can
/// never require its target to be published first and must not become a
/// release-graph edge (see [`collect_workspace_dep_names_from_table`]).
///
/// The `version` value is checked for being an actual string rather than for
/// mere key presence, so a malformed entry is classified as versionless rather
/// than silently trusted.
fn is_version_bearing_dep(value: &toml_edit::Item) -> bool {
    if value.as_str().is_some() {
        return true;
    }
    value
        .as_table_like()
        .and_then(|table| table.get("version"))
        .and_then(toml_edit::Item::as_str)
        .is_some()
}

/// Resolve the package name represented by a Cargo dependency entry.
///
/// Cargo dependency keys may be aliases (`alias = { package = "real-name", ... }`).
/// In that case graph edges and workspace version updates must bind to the
/// package named by `package`; ordinary dependencies continue to use their key.
pub(crate) fn effective_dependency_name<'a>(
    dependency_key: &'a str,
    value: &'a toml_edit::Item,
) -> &'a str {
    value
        .as_table_like()
        .and_then(|dependency| dependency.get("package"))
        .and_then(toml_edit::Item::as_str)
        .unwrap_or(dependency_key)
}

/// Whether a Cargo dependency table survives `cargo package` wholesale, or is
/// filtered down to its version-bearing entries first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DependencyTableKind {
    /// `[dependencies]` / `[build-dependencies]`: every entry reaches the
    /// published manifest, so a local edge always constrains publish order.
    /// (A path-only entry here makes the package unpublishable outright, which
    /// is Cargo's error to report — not a reason to drop the edge.)
    Published,
    /// `[dev-dependencies]`: Cargo erases every entry that carries no version
    /// requirement, so only the version-bearing ones constrain publish order.
    Dev,
}

/// Dependency tables Cargo can use for local package edges, each paired with
/// how packaging treats it.
const CARGO_DEPENDENCY_TABLES: &[(&str, DependencyTableKind)] = &[
    ("dependencies", DependencyTableKind::Published),
    ("dev-dependencies", DependencyTableKind::Dev),
    ("build-dependencies", DependencyTableKind::Published),
];

/// `workspace_versioned` is the nearest workspace root's
/// [`workspace_versioned_dependencies`] set, or `None` when no root applies.
/// It is keyed by the `[workspace.dependencies]` KEY, which is exactly what a
/// member repeats as `key.workspace = true`, so an aliased entry
/// (`key = { package = "real" }`) still resolves.
fn collect_workspace_dep_names_from_table<'a>(
    deps: &'a dyn toml_edit::TableLike,
    kind: DependencyTableKind,
    workspace_versioned: Option<&HashSet<String>>,
    dep_names: &mut Vec<&'a str>,
) {
    for (dep_name, value) in deps.iter() {
        // A dev entry only reaches the published manifest when it carries a
        // version requirement; for an inherited entry that requirement lives on
        // the workspace root, so the two shapes are decided from different
        // places.
        let is_release_edge = if crate::is_workspace_marker(value) {
            kind == DependencyTableKind::Published
                || workspace_versioned.is_some_and(|versioned| versioned.contains(dep_name))
        } else if is_local_path_dep(value) {
            kind == DependencyTableKind::Published || is_version_bearing_dep(value)
        } else {
            false
        };
        if is_release_edge {
            dep_names.push(effective_dependency_name(dep_name, value));
        }
    }
}

/// Collect names of dependency entries declared as `dep = { workspace = true }`
/// — the shape used by workspace members to inherit dependency versions from
/// `[workspace.dependencies]`.
///
/// Previously open-coded inside `visit`; extracted so the same
/// `dep_names` list feeds every branch (workspace / inherits-workspace-
/// version / plain-package) through one code path. It checks top-level Cargo
/// dependency tables and target-specific dependency tables so dev, build, and
/// platform-only local edges still feed publish ordering and reverse updates.
///
/// Matches the `package_str` / `workspace_package_str` sibling-helper
/// idiom already established in this file.
fn workspace_dep_names<'a>(
    doc: &'a toml_edit::DocumentMut,
    workspace_versioned: Option<&HashSet<String>>,
) -> Vec<&'a str> {
    let mut dep_names = Vec::new();

    for (table_name, kind) in CARGO_DEPENDENCY_TABLES {
        if let Some(deps) = doc.get(table_name).and_then(toml_edit::Item::as_table_like) {
            collect_workspace_dep_names_from_table(
                deps,
                *kind,
                workspace_versioned,
                &mut dep_names,
            );
        }
    }

    if let Some(targets) = doc.get("target").and_then(toml_edit::Item::as_table_like) {
        for (_, target) in targets.iter() {
            let Some(target_table) = target.as_table_like() else {
                continue;
            };
            for (table_name, kind) in CARGO_DEPENDENCY_TABLES {
                if let Some(deps) = target_table
                    .get(table_name)
                    .and_then(toml_edit::Item::as_table_like)
                {
                    collect_workspace_dep_names_from_table(
                        deps,
                        *kind,
                        workspace_versioned,
                        &mut dep_names,
                    );
                }
            }
        }
    }

    dep_names
}

fn workspace_dependency_aliases(doc: &toml_edit::DocumentMut) -> HashMap<String, String> {
    doc.get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml_edit::Item::as_table_like)
        .map(|dependencies| {
            dependencies
                .iter()
                .filter(|(_, dependency)| is_local_path_dep(dependency))
                .filter_map(|(dependency_key, dependency)| {
                    let package_name = effective_dependency_name(dependency_key, dependency);
                    (package_name != dependency_key)
                        .then(|| (dependency_key.to_string(), package_name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Keys of the `[workspace.dependencies]` entries that carry a version
/// requirement.
///
/// Keyed by the entry's KEY rather than its aliased package name, because that
/// is what an inheriting member repeats as `key.workspace = true`.
///
/// Recorded POSITIVELY on purpose: an entry that is absent, malformed, or lives
/// in a root this walk never discovered simply does not appear, which
/// classifies an inheriting dev-dependency as erased-at-package-time. That is
/// the same answer Cargo gives for inheritance it cannot resolve, and it keeps
/// an unknown root from silently minting release-graph edges.
fn workspace_versioned_dependencies(doc: &toml_edit::DocumentMut) -> HashSet<String> {
    doc.get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml_edit::Item::as_table_like)
        .map(|dependencies| {
            dependencies
                .iter()
                .filter(|(_, dependency)| is_version_bearing_dep(dependency))
                .map(|(dependency_key, _)| dependency_key.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn nearest_workspace_entry<'a, V>(
    map: &'a HashMap<PathBuf, V>,
    member_path: &Path,
) -> Option<(&'a PathBuf, &'a V)> {
    map.iter()
        .filter(|(root_path, _)| {
            root_path
                .parent()
                .is_some_and(|root_dir| member_path.starts_with(root_dir))
        })
        .max_by_key(|(root_path, _)| root_path.components().count())
}

/// The git repository root that `manifest_path` was discovered under.
///
/// `relative_path` MUST be the manifest's repo-root-relative path, so its
/// component count is exactly the number of ancestor steps from
/// `manifest_path` back up to that root. Callers use the result as the
/// ancestor-walk boundary that stops discovery from adopting a `Cargo.toml`
/// living above the repository. Falls back to `manifest_path` itself when the
/// two paths disagree and the walk runs out of ancestors.
fn repository_root(manifest_path: &Path, relative_path: &Path) -> PathBuf {
    manifest_path
        .ancestors()
        .nth(relative_path.components().count())
        .map_or_else(|| manifest_path.to_path_buf(), Path::to_path_buf)
}

/// The ancestor manifest that [`RustProjectFinder::discover_workspace_root_for_member`]
/// accepted as a pending member's workspace root, handed back to `finalize`
/// so the (pure) search stays separate from the state mutation it feeds.
///
/// The parsed manifest travels with the path because `finalize` reads four
/// more fields out of the very same bytes (`[package].name`,
/// `[package].version`, `[package].publish`, `[workspace.package].publish`);
/// returning only the path would force a second read+parse of a file the walk
/// has already decoded.
#[derive(Debug)]
struct DiscoveredWorkspaceRoot {
    /// Absolute path of the accepted ancestor `Cargo.toml`.
    root_path: PathBuf,
    /// Its parsed contents, already validated to carry
    /// `[workspace.package].version`.
    manifest: toml_edit::DocumentMut,
    /// That `[workspace.package].version` value.
    workspace_version: String,
}

/// Everything a member may inherit from one discovered workspace root.
///
/// These two answers are decoded from the same root manifest bytes at the same
/// moment ([`RustProjectFinder::record_workspace_root`]) and are always looked
/// up for the same member path, so they live in one value under one root key.
/// Keeping them in two parallel maps meant every visited member manifest paid
/// two independent O(roots) [`nearest_workspace_entry`] scans — each of which
/// re-walks every candidate root's path components — to learn two halves of a
/// single answer.
#[derive(Debug)]
struct WorkspaceRootInfo {
    /// `[workspace.package].publish`, so a member writing
    /// `publish.workspace = true` can inherit it. An entry is recorded for
    /// EVERY discovered root — including roots that declare no `publish`
    /// (value `true`) — because the nearest root has to win over a shallower
    /// one that does declare it.
    publishable: bool,
    /// `[workspace.dependencies]` entries whose key is an alias for a
    /// differently named local-path package (`alias = { package = "real" }`),
    /// mapping alias key to real package name.
    aliases: HashMap<String, String>,
    /// Keys of the `[workspace.dependencies]` entries carrying a version
    /// requirement, so a member's `key.workspace = true` inside
    /// `[dev-dependencies]` can be told apart from the path-only inheritance
    /// Cargo erases at package time. See [`workspace_versioned_dependencies`].
    versioned_dependencies: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct RustProjectFinder {
    projects: HashMap<PathBuf, Project>,
    workspace_package_versions: HashMap<PathBuf, String>,
    /// Per-discovered-workspace-root inheritance data. Its presence check is
    /// also what lets `discover_workspace_dependency_aliases_for_member` stop
    /// walking early.
    workspace_roots: HashMap<PathBuf, WorkspaceRootInfo>,
    non_workspace_manifest_candidates: HashSet<PathBuf>,
    inherited_workspace_members: HashMap<PathBuf, InheritedWorkspaceMembers>,
    pending_workspace_packages: Vec<PendingWorkspacePackage>,
    /// Hashed membership index over `pending_workspace_packages`' `abs_path`s.
    /// The `Vec` remains the authoritative resolution-order source; this set
    /// only answers "already pending?" in `visit` in O(1) instead of a linear
    /// `PathBuf` scan over every deferred member (which made discovery
    /// O(members^2) in a Cargo workspace where nearly every crate inherits
    /// `version.workspace = true`). Kept in lockstep with the `Vec`: inserted
    /// at the single push site, cleared alongside the `std::mem::take` in
    /// `finalize`, which then hands the taken `Vec` to
    /// `resolve_pending_workspace_packages`.
    pending_workspace_paths: HashSet<PathBuf>,
}

impl RustProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn nearest_workspace_package(&self, member_path: &Path) -> Option<(String, PathBuf)> {
        nearest_workspace_entry(&self.workspace_package_versions, member_path)
            .map(|(root_path, version)| (version.clone(), root_path.clone()))
    }

    /// Everything the nearest discovered workspace root above `member_path`
    /// lets that member inherit, or `None` when no root was found. One scan
    /// answers both the inherited-`publish` and the dependency-alias question.
    fn nearest_workspace_root_info(&self, member_path: &Path) -> Option<&WorkspaceRootInfo> {
        nearest_workspace_entry(&self.workspace_roots, member_path).map(|(_, info)| info)
    }

    /// Record everything a member may later inherit from `root_path` as the
    /// single root-keyed entry every discovery site writes.
    fn record_workspace_root(&mut self, root_path: PathBuf, doc: &toml_edit::DocumentMut) {
        self.workspace_roots.insert(
            root_path,
            WorkspaceRootInfo {
                publishable: workspace_package_publishable_by_default(doc),
                aliases: workspace_dependency_aliases(doc),
                versioned_dependencies: workspace_versioned_dependencies(doc),
            },
        );
    }

    async fn discover_workspace_dependency_aliases_for_member(
        &mut self,
        member_path: &Path,
        relative_path: &Path,
    ) -> Result<()> {
        let repository_root = repository_root(member_path, relative_path);

        for ancestor in member_path.ancestors().skip(2) {
            if !ancestor.starts_with(&repository_root) {
                return Ok(());
            }
            let candidate = ancestor.join("Cargo.toml");
            if self.workspace_roots.contains_key(&candidate) {
                return Ok(());
            }
            if !self.non_workspace_manifest_candidates.contains(&candidate) {
                let parsed = read_cargo_toml_if_absent_ok(&candidate).await?;
                if let Some(parsed) = parsed.filter(|parsed| parsed.get("workspace").is_some()) {
                    self.record_workspace_root(candidate, &parsed);
                    return Ok(());
                }
                self.non_workspace_manifest_candidates.insert(candidate);
            }
        }
        Ok(())
    }

    /// Find the nearest ancestor `Cargo.toml` above `abs_path` that declares a
    /// `[workspace.package].version`, stopping at `git_root` so discovery never
    /// adopts a manifest living outside the repository.
    ///
    /// Read-only on `self` by design: the caller ([`Self::finalize`]) owns every
    /// mutation the acceptance implies, so the walk cannot half-record a root it
    /// then fails to insert. Returns `Ok(None)` when the walk leaves the
    /// repository, when it reaches an already-known root (that member is already
    /// resolvable through `workspace_package_versions`), or when it runs out of
    /// ancestors.
    ///
    /// `rejected_candidates` is the caller's cross-member memo of ancestor
    /// manifests this walk already read and rejected — either the file does not
    /// exist, or it exists without a `[workspace.package].version`. Sibling
    /// members share almost all of their ancestor chain, so without it every
    /// member pays one failing `read` per missing intermediate manifest and one
    /// full read+TOML-parse per present-but-rejected ancestor (e.g. a root that
    /// has `[workspace]` but no `[workspace.package].version`). Skipping a
    /// memoized candidate is byte-identical to re-reading it: this fn touches no
    /// finder state, and acceptance depends purely on the candidate file's
    /// bytes, which cannot change mid-`finalize`.
    ///
    /// Deliberately NOT `self.non_workspace_manifest_candidates`: that field's
    /// invariant is the narrower "manifest has no `[workspace]` table" one relied
    /// on by [`Self::discover_workspace_dependency_aliases_for_member`], and
    /// widening it would make that walk skip real workspace roots.
    ///
    /// # Errors
    /// Propagates a malformed ancestor manifest or a permission problem with its
    /// path context attached; a merely missing ancestor `Cargo.toml` is an
    /// ordinary negative answer (see [`read_cargo_toml_if_absent_ok`]).
    async fn discover_workspace_root_for_member(
        &self,
        abs_path: &Path,
        git_root: &Path,
        rejected_candidates: &mut HashSet<PathBuf>,
    ) -> Result<Option<DiscoveredWorkspaceRoot>> {
        for parent in abs_path.ancestors().skip(2) {
            if !parent.starts_with(git_root) {
                return Ok(None);
            }
            let candidate = parent.join("Cargo.toml");
            if self.workspace_package_versions.contains_key(&candidate) {
                return Ok(None);
            }
            // `continue`, never stop: a rejected ancestor says nothing about
            // the ancestors above it, which must still be walked.
            if rejected_candidates.contains(&candidate) {
                continue;
            }
            let parsed = read_cargo_toml_if_absent_ok(&candidate).await?;
            if let Some((manifest, workspace_version)) = parsed.and_then(|parsed| {
                workspace_package_str(&parsed, "version")
                    .map(|workspace_version| (parsed, workspace_version))
            }) {
                return Ok(Some(DiscoveredWorkspaceRoot {
                    root_path: candidate,
                    manifest,
                    workspace_version,
                }));
            }
            rejected_candidates.insert(candidate);
        }
        Ok(None)
    }

    fn insert_workspace_member(
        &mut self,
        package: PendingWorkspacePackage,
        workspace_package_version: Option<String>,
        workspace_root_path: Option<PathBuf>,
    ) {
        let PendingWorkspacePackage {
            name,
            abs_path,
            relative_path,
            mut dependencies,
            publishable_by_default,
        } = package;
        // One lookup for both users below: the alias-collection pass and the
        // dependency rewrite pass previously probed the alias map with the same
        // `PathBuf` key twice, so the second probe (and its path hashing) is
        // elided here.
        let root_aliases = workspace_root_path
            .as_ref()
            .and_then(|root| self.workspace_roots.get(root))
            .map(|info| &info.aliases);
        if let (Some(root_path), Some(package_name)) = (workspace_root_path.as_ref(), name.as_ref())
        {
            let aliases = root_aliases
                .into_iter()
                .flat_map(|aliases| aliases.iter())
                .filter(|(_, aliased_package_name)| *aliased_package_name == package_name)
                .map(|(alias, _)| alias.clone())
                .collect::<Vec<_>>();
            // Probe before allocating: every inherited member of one workspace
            // shares the same root key, so only the first call misses and the
            // `entry(root_path.clone())` clone would be wasted on every later
            // hit. Same policy as `crates/utils/src/gen_update_map.rs:271`.
            let inherited_members =
                if let Some(existing) = self.inherited_workspace_members.get_mut(root_path) {
                    existing
                } else {
                    self.inherited_workspace_members
                        .entry(root_path.clone())
                        .or_default()
                };
            let mut inherited_members = crate::workspace::lock_recovering(inherited_members);
            inherited_members.record(package_name, aliases);
        }
        if let Some(aliases) = root_aliases {
            for dependency in &mut dependencies {
                if let Some(package_name) = aliases.get(dependency) {
                    dependency.clone_from(package_name);
                }
            }
        }
        let mut pkg = RustPackage::new_with_workspace_version(
            name,
            workspace_package_version,
            abs_path.clone(),
            relative_path,
            workspace_root_path,
        )
        .with_publishable_by_default(publishable_by_default);
        for dependency in dependencies {
            pkg.add_dependency(&dependency);
        }
        self.projects
            .insert(abs_path, Project::Package(Box::new(pkg)));
    }

    /// Resolves the members `finalize` already took out of
    /// `pending_workspace_packages`; the caller owns the `Vec` (and has
    /// already cleared `pending_workspace_paths` in lockstep) so it can read
    /// the member paths by reference before handing ownership over here.
    fn resolve_pending_workspace_packages(&mut self, pending: Vec<PendingWorkspacePackage>) {
        for package in pending {
            let (version, root_path) = self
                .nearest_workspace_package(&package.abs_path)
                .map_or((None, None), |(version, root_path)| {
                    (Some(version), Some(root_path))
                });
            self.insert_workspace_member(package, version, root_path);
        }
    }
}

#[async_trait]
impl ProjectFinder for RustProjectFinder {
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        if !self.matches_project_file(path).await? {
            return Ok(());
        }
        // The already-discovered half now goes through the shared
        // `ProjectFinder::contains_project` probe; the extra
        // pending-member check is Rust-only (members whose version is still
        // deferred to `resolve_pending_workspace_packages`) and stays
        // open-coded here, so `should_visit_manifest` is not usable for this
        // finder. Both halves are now O(1) hashed probes.
        if self.contains_project(path) || self.pending_workspace_paths.contains(path) {
            return Ok(());
        }
        // read Cargo.toml
        let (_cargo_toml_raw, cargo_toml) = crate::read_and_parse_cargo_toml(path).await?;
        let publish_default = package_publish_default(&cargo_toml);
        let is_workspace = cargo_toml.get("workspace").is_some();

        if is_workspace {
            // Record BEFORE the nearest-root scan below, so a hybrid root
            // (`[workspace]` + `[package]`) resolves its own dependency
            // inheritance against its OWN `[workspace.dependencies]` instead of
            // a parent workspace's. `nearest_workspace_entry` keys on the root
            // manifest's directory, which is a prefix of the manifest path
            // itself, so the freshly recorded entry is the deepest match and
            // wins. Recording here is also idempotent with respect to the
            // workspace arm below, which no longer re-records it.
            self.record_workspace_root(path.to_path_buf(), &cargo_toml);
        } else {
            self.discover_workspace_dependency_aliases_for_member(path, relative_path)
                .await?;
        }

        // ONE nearest-root scan feeds both inherited answers below. Nothing
        // between here and the `dep_names` collection mutates `workspace_roots`,
        // so a single borrow is equivalent to the two independent scans this
        // replaces — at half the linear walks over the recorded roots.
        let nearest_root = self.nearest_workspace_root_info(path);

        // A manifest carrying `[workspace]` IS its own workspace root, so an
        // inherited `publish` resolves against its own `[workspace.package]`.
        // Every other manifest resolves against the nearest root, which the
        // discovery walk above has just guaranteed is recorded when one exists
        // on disk — no visit ordering dependency, and no root means the
        // permissive default.
        let publishable_by_default = if is_workspace {
            publish_default.resolve(|| Some(workspace_package_publishable_by_default(&cargo_toml)))
        } else {
            publish_default.resolve(|| nearest_root.map(|root| root.publishable))
        };

        // Collect workspace dependencies for this file — the same
        // `dep_names` list feeds every branch below (workspace /
        // inherits-workspace-version / plain-package).
        let dep_names: Vec<String> = workspace_dep_names(
            &cargo_toml,
            nearest_root.map(|root| &root.versioned_dependencies),
        )
        .into_iter()
        .map(|dependency_name| {
            nearest_root
                .and_then(|root| root.aliases.get(dependency_name))
                .map_or_else(|| dependency_name.to_string(), Clone::clone)
        })
        .collect();

        // if workspace
        if is_workspace {
            let path_key = path.to_path_buf();

            // Read [workspace.package].version if present
            let ws_pkg_version = workspace_package_str(&cargo_toml, "version");
            if let Some(version) = ws_pkg_version {
                self.workspace_package_versions
                    .insert(path_key.clone(), version);
            }

            // A visited workspace root's own version: prefer its `[package].version`
            // string, but fall back to `[workspace.package].version` for a virtual
            // workspace (no `[package]`) or a hybrid root whose `[package]` inherits
            // via `version.workspace = true` (a table, so `package_str` → `None`).
            // Without this fallback the constructed `RustWorkspace` reports
            // `version = None`, and a later inherited bump promoted onto the root
            // path would rewrite from `0.0.0`, downgrading the real version. This
            // aligns `visit` with the same fallback `finalize` already applies.
            let version = package_str(&cargo_toml, "version")
                .or_else(|| workspace_package_str(&cargo_toml, "version"));
            let name = package_str(&cargo_toml, "name");
            let inherited_workspace_members = self
                .inherited_workspace_members
                .entry(path_key.clone())
                .or_default()
                .clone();
            // Hoist the shared `PathBuf` into one binding: `path_key` seeds
            // both the `RustWorkspace::new(...)` constructor slot and the
            // `self.projects.insert(...)` map key. Mirror of the same
            // pattern already used by the `inherits_workspace` and
            // plain-package `else` arms below, and by
            // `crates/csharp/src/finder.rs::visit` /
            // `crates/java/src/finder.rs::visit`. Byte-identical
            // semantics — the same `PathBuf` bytes flow into
            // `RustWorkspace::new` and the map key, just materialized
            // once up front.
            let mut project = Project::Workspace(Box::new(
                RustWorkspace::new_with_inherited_workspace_members(
                    name,
                    version,
                    path_key.clone(),
                    relative_path.to_path_buf(),
                    inherited_workspace_members,
                    publishable_by_default,
                ),
            ));
            for dep_name in &dep_names {
                project.add_dependency(dep_name);
            }
            self.projects.insert(path_key, project);
        } else {
            // Check if version.workspace = true — same table-like +
            // `workspace = true` shape as `workspace_dep_names`
            // above, so both call sites share the [`is_workspace_marker`]
            // decoder. Byte-identical to the previous
            // six-`.and_then` chain because `is_some_and(...)`
            // short-circuits on the same `None` cases and its final
            // `.unwrap_or(false)` matches.
            let inherits_workspace = cargo_toml
                .get("package")
                .and_then(|p| p.get("version"))
                .is_some_and(crate::is_workspace_marker);

            let name = package_str(&cargo_toml, "name");

            // Hoist BOTH shared `PathBuf`s once for every non-workspace
            // arm: `path_key` / `relative_path_key` seed both the
            // constructor slot (`RustPackage::new*` /
            // `PendingWorkspacePackage`) AND the
            // `self.projects.insert(...)` map key (for `path_key`),
            // mirroring the same pattern already used in the
            // workspace arm above and by every peer finder (Node,
            // Python, CSharp, Java, Dart). Each branch clones each
            // key into non-final slots and moves it into the LAST-
            // used slot — one `PathBuf` allocation per key per visit
            // instead of two-to-three, byte-identical output.
            let path_key = path.to_path_buf();
            let relative_path_key = relative_path.to_path_buf();

            if inherits_workspace {
                self.pending_workspace_paths.insert(path_key.clone());
                self.pending_workspace_packages
                    .push(PendingWorkspacePackage {
                        name,
                        abs_path: path_key,
                        relative_path: relative_path_key,
                        dependencies: dep_names,
                        publishable_by_default,
                    });
            } else {
                let version = package_str(&cargo_toml, "version");
                let mut project = Project::Package(Box::new(
                    RustPackage::new(name, version, path_key.clone(), relative_path_key)
                        .with_publishable_by_default(publishable_by_default),
                ));
                for dep_name in &dep_names {
                    project.add_dependency(dep_name);
                }
                self.projects.insert(path_key, project);
            }
        }
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        // Take ownership of the deferred members up front. That ends the
        // borrow of `self`, so the discovery loop below can read each member's
        // paths BY REFERENCE while still mutating other `self` fields —
        // replacing the throwaway `Vec` of deep-copied `abs_path` /
        // `relative_path` pairs this used to build for every pending member.
        // `pending_workspace_paths` is cleared here to keep the documented
        // lockstep invariant with the `Vec`.
        let pending = std::mem::take(&mut self.pending_workspace_packages);
        self.pending_workspace_paths.clear();

        // Cross-member memo of already-rejected ancestor manifests, owned here so
        // sibling members that share an ancestor chain read each dead candidate
        // once for the whole `finalize`. See
        // [`Self::discover_workspace_root_for_member`] for the rejection rule and
        // why re-reading a memoized candidate would be redundant rather than
        // different.
        let mut rejected_candidates: HashSet<PathBuf> = HashSet::new();

        // Roots can be omitted by ignore patterns, so discover the nearest root
        // independently for every unresolved member.
        for package in &pending {
            let git_root = repository_root(&package.abs_path, &package.relative_path);
            let Some(DiscoveredWorkspaceRoot {
                root_path,
                manifest,
                workspace_version,
            }) = self
                .discover_workspace_root_for_member(
                    &package.abs_path,
                    &git_root,
                    &mut rejected_candidates,
                )
                .await?
            else {
                continue;
            };

            self.workspace_package_versions
                .insert(root_path.clone(), workspace_version.clone());
            self.record_workspace_root(root_path.clone(), &manifest);

            // Insert synthetic workspace project so apply_updates() can find it
            let ws_name = package_str(&manifest, "name");
            let ws_pkg_version = package_str(&manifest, "version");
            // This manifest is itself the workspace root, so a hybrid
            // `[package] publish.workspace = true` inherits from the
            // `[workspace.package].publish` in these very bytes.
            let publishable_by_default = package_publish_default(&manifest)
                .resolve(|| Some(workspace_package_publishable_by_default(&manifest)));
            let ws_relative_path = root_path
                .strip_prefix(&git_root)
                .unwrap_or(Path::new("Cargo.toml"))
                .to_path_buf();

            let inherited_workspace_members = self
                .inherited_workspace_members
                .entry(root_path.clone())
                .or_default()
                .clone();
            let workspace = RustWorkspace::new_with_inherited_workspace_members(
                ws_name,
                // For virtual workspaces (no [package]), use [workspace.package].version
                ws_pkg_version.or(Some(workspace_version)),
                root_path.clone(),
                ws_relative_path,
                inherited_workspace_members,
                publishable_by_default,
            );
            self.projects
                .insert(root_path, Project::Workspace(Box::new(workspace)));
        }

        self.resolve_pending_workspace_packages(pending);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::{ChangePackResultLog, Project, UpdateType};
    use rstest::rstest;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    fn write_inherited_version_workspace(
        root: &Path,
        package_name: &str,
        version: &str,
    ) -> (PathBuf, PathBuf) {
        fs::create_dir_all(root).unwrap();
        let workspace_toml = root.join("Cargo.toml");
        fs::write(
            &workspace_toml,
            format!(
                r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "{version}"
"#
            ),
        )
        .unwrap();

        let package_dir = root.join("crates").join(package_name);
        fs::create_dir_all(&package_dir).unwrap();
        let package_toml = package_dir.join("Cargo.toml");
        fs::write(
            &package_toml,
            format!(
                r#"[package]
name = "{package_name}"
version.workspace = true
"#
            ),
        )
        .unwrap();

        (workspace_toml, package_toml)
    }

    fn write_inherited_version_fanout_workspace(
        root: &Path,
        package_name: &str,
        version: &str,
    ) -> (PathBuf, PathBuf) {
        fs::create_dir_all(root).unwrap();
        let workspace_toml = root.join("Cargo.toml");
        fs::write(
            &workspace_toml,
            format!(
                r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "{version}"

[workspace.dependencies]
{package_name} = {{ path = "crates/{package_name}", version = "{version}" }}
"#
            ),
        )
        .unwrap();

        let package_dir = root.join("crates").join(package_name);
        fs::create_dir_all(&package_dir).unwrap();
        let package_toml = package_dir.join("Cargo.toml");
        fs::write(
            &package_toml,
            format!(
                r#"[package]
name = "{package_name}"
version.workspace = true
"#
            ),
        )
        .unwrap();

        (workspace_toml, package_toml)
    }

    async fn bump_workspace(finder: &mut RustProjectFinder, relative_path: &Path) {
        let workspace = finder
            .projects_mut()
            .into_iter()
            .find(|project| {
                matches!(project, Project::Workspace(_)) && project.relative_path() == relative_path
            })
            .expect("workspace project should be discovered");
        workspace.update_version(UpdateType::Patch).await.unwrap();
    }

    async fn visit_single_manifest(content: &str) -> (TempDir, RustProjectFinder) {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, content).unwrap();
        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        (temp_dir, finder)
    }

    // Both `RustProjectFinder::new()` and `RustProjectFinder::default()` must
    // yield the same empty, `Cargo.toml`-scoped finder.
    #[rstest]
    #[case(RustProjectFinder::new())]
    #[case(RustProjectFinder::default())]
    fn test_rust_project_finder_construction(#[case] finder: RustProjectFinder) {
        assert_eq!(finder.project_files(), &["Cargo.toml"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let pkg = projects[0].expect_package();
        assert_eq!(pkg.name(), Some("test-package"));
        assert_eq!(pkg.version(), Some("1.0.0"));
        assert!(pkg.is_publishable_by_default());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_standalone_publish_false_is_not_publishable_by_default() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[package]
name = "private-package"
version = "1.0.0"
publish = false
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(!projects[0].is_publishable_by_default());
    }

    #[tokio::test]
    async fn test_rust_project_finder_publish_registry_list_remains_publishable_by_default() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[package]
name = "registry-package"
version = "1.0.0"
publish = ["internal"]
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].is_publishable_by_default());
    }

    // `publish = []` is an empty registry allow-list: Cargo refuses to publish
    // such a crate exactly like `publish = false`, so the finder must not
    // advertise it as publishable.
    #[tokio::test]
    async fn test_rust_project_finder_empty_publish_list_is_not_publishable_by_default() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[package]
name = "empty-allow-list-package"
version = "1.0.0"
publish = []
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(!projects[0].is_publishable_by_default());
    }

    #[tokio::test]
    async fn test_rust_project_finder_multi_registry_publish_list_remains_publishable_by_default() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[package]
name = "multi-registry-package"
version = "1.0.0"
publish = ["internal", "mirror"]
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].is_publishable_by_default());
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[package]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let ws = projects[0].expect_workspace();
        assert_eq!(ws.name(), Some("test-workspace"));
        assert_eq!(ws.version(), Some("1.0.0"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_hybrid_root_publish_false_is_not_publishable_by_default() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[workspace]
members = ["crates/*"]

[package]
name = "private-workspace"
version = "1.0.0"
publish = false
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(matches!(projects[0], Project::Workspace(_)));
        assert!(!projects[0].is_publishable_by_default());
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_workspace_without_package() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let ws = projects[0].expect_workspace();
        assert_eq!(ws.name(), None);
        assert_eq!(ws.version(), None);
        assert!(!ws.is_publishable_by_default());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_inherited_version_member_publish_false_is_not_publishable_by_default() {
        let temp_dir = TempDir::new().unwrap();
        let (workspace_toml, package_toml) =
            write_inherited_version_workspace(temp_dir.path(), "private-member", "1.0.0");
        fs::write(
            &package_toml,
            r#"[package]
name = "private-member"
version.workspace = true
publish = false
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(
                &package_toml,
                &PathBuf::from("crates/private-member/Cargo.toml"),
            )
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let member = finder
            .projects()
            .into_iter()
            .find(|project| project.name() == Some("private-member"))
            .expect("inherited-version member should be discovered");
        assert!(matches!(member, Project::Package(_)));
        assert_eq!(member.version(), Some("1.0.0"));
        assert!(!member.is_publishable_by_default());
    }

    /// Run a full discovery pass over a two-manifest workspace and report
    /// whether the member came out publishable by default.
    ///
    /// `workspace_package_extra` is appended verbatim to the root's
    /// `[workspace.package]` table (so a test can add `publish = ...` or leave
    /// it out entirely) and `member_package_body` is appended verbatim to the
    /// member's `[package]` table after its `name`.
    async fn discover_member_publishable(
        workspace_package_extra: &str,
        member_package_body: &str,
    ) -> bool {
        let temp_dir = TempDir::new().unwrap();
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            format!(
                "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"1.0.0\"\n{workspace_package_extra}"
            ),
        )
        .unwrap();

        let member_dir = temp_dir.path().join("crates").join("member");
        fs::create_dir_all(&member_dir).unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            format!("[package]\nname = \"member\"\n{member_package_body}"),
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&member_toml, &PathBuf::from("crates/member/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let publishable = finder
            .projects()
            .into_iter()
            .find(|project| project.name() == Some("member"))
            .expect("member should be discovered")
            .is_publishable_by_default();

        temp_dir.close().unwrap();
        publishable
    }

    // `publish` is an inheritable Cargo [package] field: a member writing
    // `publish.workspace = true` takes the workspace root's
    // [workspace.package].publish. Both disabling shapes (`false` and the empty
    // allow-list) must therefore reach the member, and both enabling shapes
    // plus an absent key must leave it publishable.
    #[tokio::test]
    async fn test_inherited_publish_member_resolves_workspace_publish_value() {
        for (workspace_publish, expected) in [
            ("publish = false\n", false),
            ("publish = []\n", false),
            ("publish = true\n", true),
            ("publish = [\"internal\"]\n", true),
            ("", true),
        ] {
            let publishable = discover_member_publishable(
                workspace_publish,
                "version.workspace = true\npublish.workspace = true\n",
            )
            .await;
            assert_eq!(
                publishable, expected,
                "member inheriting publish under root {workspace_publish:?}"
            );
        }
    }

    // The inline-table spelling of the same inherit marker must behave
    // identically to the dotted-key one.
    #[tokio::test]
    async fn test_inherited_publish_inline_table_marker_resolves_workspace_publish_value() {
        assert!(
            !discover_member_publishable(
                "publish = false\n",
                "version.workspace = true\npublish = { workspace = true }\n",
            )
            .await
        );
    }

    // A member that does NOT inherit its version still inherits `publish`; the
    // answer must not depend on the version-deferral path.
    #[tokio::test]
    async fn test_inherited_publish_member_with_literal_version_honours_workspace_root() {
        assert!(
            !discover_member_publishable(
                "publish = false\n",
                "version = \"2.0.0\"\npublish.workspace = true\n",
            )
            .await
        );
    }

    // Standalone answers are untouched by workspace-level `publish`: a literal
    // non-empty registry list stays publishable even under a root that disables
    // publishing, and a literal `false` stays unpublishable under a root that
    // enables it.
    #[tokio::test]
    async fn test_standalone_publish_answers_ignore_workspace_publish_value() {
        assert!(
            discover_member_publishable(
                "publish = false\n",
                "version.workspace = true\npublish = [\"internal\"]\n",
            )
            .await
        );
        assert!(
            !discover_member_publishable(
                "publish = true\n",
                "version.workspace = true\npublish = false\n",
            )
            .await
        );
        assert!(
            discover_member_publishable("publish = false\n", "version.workspace = true\n").await
        );
    }

    // A hybrid root ([workspace] + [package]) is its own workspace root, so its
    // `publish.workspace = true` resolves against the [workspace.package].publish
    // in the very same manifest.
    #[tokio::test]
    async fn test_hybrid_root_inherited_publish_resolves_from_own_workspace_package() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
publish = false

[package]
name = "hybrid-root"
version = "1.0.0"
publish.workspace = true
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(matches!(projects[0], Project::Workspace(_)));
        assert!(!projects[0].is_publishable_by_default());
    }

    // With no workspace root anywhere above it, an inherited `publish` has
    // nothing to resolve against and falls back to the permissive default
    // rather than being reported as unpublishable.
    #[tokio::test]
    async fn test_inherited_publish_without_workspace_root_defaults_to_publishable() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[package]
name = "orphan"
version = "1.0.0"
publish.workspace = true
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].is_publishable_by_default());
    }

    // The NEAREST workspace root wins: an inner root that says nothing about
    // `publish` must not let an outer root's `publish = false` leak through.
    #[tokio::test]
    async fn test_inherited_publish_uses_nearest_workspace_root() {
        let temp_dir = TempDir::new().unwrap();
        let outer_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &outer_toml,
            "[workspace]\nmembers = [\"inner\"]\n\n[workspace.package]\nversion = \"1.0.0\"\npublish = false\n",
        )
        .unwrap();

        let inner_dir = temp_dir.path().join("inner");
        let member_dir = inner_dir.join("crates").join("member");
        fs::create_dir_all(&member_dir).unwrap();
        fs::write(
            inner_dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"2.0.0\"\n",
        )
        .unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            "[package]\nname = \"member\"\nversion = \"2.0.0\"\npublish.workspace = true\n",
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(
                &member_toml,
                &PathBuf::from("inner/crates/member/Cargo.toml"),
            )
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let member = finder
            .projects()
            .into_iter()
            .find(|project| project.name() == Some("member"))
            .expect("member should be discovered");
        assert!(member.is_publishable_by_default());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_workspace_uses_workspace_package_version() {
        // A virtual workspace root (no [package]) that declares its version only
        // via [workspace.package].version. When VISITED directly, the finder must
        // report that version on the Workspace project — mirroring the fallback
        // finalize() already applies — so a later inherited bump promoted onto the
        // root path never rewrites from a phantom 0.0.0 and downgrades it.
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let ws = projects[0].expect_workspace();
        // No [package] name, but the version is inherited from
        // [workspace.package].version via the new fallback.
        assert_eq!(ws.name(), None);
        assert_eq!(ws.version(), Some("0.1.33"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_non_cargo_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.txt");
        fs::write(&other_file, "some content").unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&other_file, &PathBuf::from("other.txt"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_directory() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Pass directory instead of file
        finder
            .visit(temp_dir.path(), &PathBuf::from("."))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        // Visit again - should not add duplicate
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    // A `version.workspace = true` member whose manifest is visited twice must
    // be deferred exactly once. The pending `Vec` stays the resolution-order
    // source, so the O(1) `pending_workspace_paths` probe has to keep it
    // duplicate-free, and both containers must be emptied together when the
    // pending members are resolved.
    #[tokio::test]
    async fn test_rust_project_finder_visit_inherited_member_twice_defers_once() {
        let temp_dir = TempDir::new().unwrap();
        let (_workspace_toml, package_toml) =
            write_inherited_version_workspace(temp_dir.path(), "member", "3.1.4");
        let relative_path = PathBuf::from("crates/member/Cargo.toml");

        let mut finder = RustProjectFinder::new();
        for _ in 0..2 {
            finder.visit(&package_toml, &relative_path).await.unwrap();
        }

        assert_eq!(finder.pending_workspace_packages.len(), 1);
        assert_eq!(
            finder.pending_workspace_paths,
            HashSet::from([package_toml.clone()])
        );

        finder.finalize().await.unwrap();

        assert!(finder.pending_workspace_packages.is_empty());
        assert!(finder.pending_workspace_paths.is_empty());
        let members = finder
            .projects()
            .into_iter()
            .filter(|project| project.name() == Some("member"))
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].version(), Some("3.1.4"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_multiple_packages() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml1 = temp_dir.path().join("package1").join("Cargo.toml");
        fs::create_dir_all(cargo_toml1.parent().unwrap()).unwrap();
        fs::write(
            &cargo_toml1,
            r#"[package]
name = "package1"
version = "1.0.0"
"#,
        )
        .unwrap();

        let cargo_toml2 = temp_dir.path().join("package2").join("Cargo.toml");
        fs::create_dir_all(cargo_toml2.parent().unwrap()).unwrap();
        fs::write(
            &cargo_toml2,
            r#"[package]
name = "package2"
version = "2.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml1, &PathBuf::from("package1/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&cargo_toml2, &PathBuf::from("package2/Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_projects_mut() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let mut_projects = finder.projects_mut();
        assert_eq!(mut_projects.len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_workspace_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
core = { workspace = true }
utils = { workspace = true }
external = "1.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let pkg = projects[0].expect_package();
        assert_eq!(pkg.name(), Some("test-package"));
        let deps = pkg.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("core"));
        assert!(deps.contains("utils"));
        // external is not a workspace dependency
        assert!(!deps.contains("external"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_path_dependencies() {
        // Given: a package manifest with a direct local-path dependency and a
        // registry dependency
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
foo = { path = "../foo", version = "0.1" }
external = "1.0"
"#,
        )
        .unwrap();

        // When: the finder visits the manifest
        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        // Then: the path dependency is tracked, the registry one is not
        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let deps = projects[0].expect_package().dependencies();
        assert_eq!(deps.len(), 1, "expected only the path dep, got {deps:?}");
        assert!(deps.contains("foo"));
        assert!(!deps.contains("external"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_resolves_inline_and_target_table_path_aliases() {
        let temp_dir = TempDir::new().unwrap();
        let core_dir = temp_dir.path().join("crates/core");
        let target_core_dir = temp_dir.path().join("crates/target-core");
        let app_dir = temp_dir.path().join("crates/app");
        fs::create_dir_all(&core_dir).unwrap();
        fs::create_dir_all(&target_core_dir).unwrap();
        fs::create_dir_all(&app_dir).unwrap();

        let core_toml = core_dir.join("Cargo.toml");
        let target_core_toml = target_core_dir.join("Cargo.toml");
        let app_toml = app_dir.join("Cargo.toml");
        fs::write(
            &core_toml,
            "[package]\nname = \"core\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(
            &target_core_toml,
            "[package]\nname = \"target-core\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(
            &app_toml,
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
renamed-core = { package = "core", path = "../core", version = "1.0.0" }

[target.'cfg(unix)'.dependencies.renamed-target-core]
package = "target-core"
path = "../target-core"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&app_toml, &PathBuf::from("crates/app/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&core_toml, &PathBuf::from("crates/core/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(
                &target_core_toml,
                &PathBuf::from("crates/target-core/Cargo.toml"),
            )
            .await
            .unwrap();

        let app = finder
            .projects()
            .into_iter()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert_eq!(app.dependencies().len(), 2);
        assert!(app.dependencies().contains("core"));
        assert!(app.dependencies().contains("target-core"));
        assert!(!app.dependencies().contains("renamed-core"));
        assert!(!app.dependencies().contains("renamed-target-core"));

        let projects = finder.projects();
        let sorted = changepacks_utils::sort_by_dependencies(projects.clone())
            .expect("fixture graph is a DAG");
        let app_index = sorted
            .iter()
            .position(|project| project.name() == Some("app"))
            .unwrap();
        assert!(
            sorted
                .iter()
                .position(|project| project.name() == Some("core"))
                .unwrap()
                < app_index
        );
        assert!(
            sorted
                .iter()
                .position(|project| project.name() == Some("target-core"))
                .unwrap()
                < app_index
        );

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core/Cargo.toml"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        changepacks_utils::apply_reverse_dependencies(&mut update_map, &projects, temp_dir.path())
            .unwrap();
        assert_eq!(
            update_map[&PathBuf::from("crates/app/Cargo.toml")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_resolves_workspace_inherited_alias_from_root_definition() {
        let temp_dir = TempDir::new().unwrap();
        let core_dir = temp_dir.path().join("crates/core");
        let app_dir = temp_dir.path().join("crates/app");
        fs::create_dir_all(&core_dir).unwrap();
        fs::create_dir_all(&app_dir).unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        let core_toml = core_dir.join("Cargo.toml");
        let app_toml = app_dir.join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
renamed-core = { package = "core", path = "crates/core" }
"#,
        )
        .unwrap();
        fs::write(
            &core_toml,
            "[package]\nname = \"core\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(
            &app_toml,
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
renamed-core = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&app_toml, Path::new("crates/app/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&workspace_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&core_toml, Path::new("crates/core/Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        let app = projects
            .iter()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert!(app.dependencies().contains("core"));
        assert!(!app.dependencies().contains("renamed-core"));

        let sorted = changepacks_utils::sort_by_dependencies(projects.clone())
            .expect("fixture graph is a DAG");
        let core_index = sorted
            .iter()
            .position(|project| project.name() == Some("core"))
            .unwrap();
        let app_index = sorted
            .iter()
            .position(|project| project.name() == Some("app"))
            .unwrap();
        assert!(core_index < app_index);

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core/Cargo.toml"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        changepacks_utils::apply_reverse_dependencies(&mut update_map, &projects, temp_dir.path())
            .unwrap();
        assert_eq!(
            update_map[&PathBuf::from("crates/app/Cargo.toml")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_does_not_resolve_alias_above_repository_boundary() {
        let temp_dir = TempDir::new().unwrap();
        let repo_dir = temp_dir.path().join("repo");
        fs::create_dir_all(&repo_dir).unwrap();

        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["repo"]

[workspace.dependencies]
renamed-core = { package = "core", path = "outside-core" }
"#,
        )
        .unwrap();
        let member_toml = repo_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
renamed-core = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        let app = projects
            .iter()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert!(app.dependencies().contains("renamed-core"));
        assert!(!app.dependencies().contains("core"));

        temp_dir.close().unwrap();
    }

    /// Visit one manifest and return the single discovered project's
    /// dependency names, sorted. Every dependency-edge test below needs the
    /// same four lines of `TempDir` + `fs::write` + `visit` + `expect_package`,
    /// so they share this one.
    async fn dependency_names_of(manifest: &str) -> Vec<String> {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, manifest).unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let mut names: Vec<String> = projects[0]
            .expect_package()
            .dependencies()
            .iter()
            .cloned()
            .collect();
        names.sort_unstable();

        temp_dir.close().unwrap();
        names
    }

    /// Write a workspace root plus one member and return the member's
    /// dependency names, sorted. The member is visited with its repo-relative
    /// path so the ancestor walk stops inside `temp_dir`.
    async fn member_dependency_names_of(root: &str, member: &str) -> Vec<String> {
        let temp_dir = TempDir::new().unwrap();
        let member_dir = temp_dir.path().join("crates").join("member");
        fs::create_dir_all(&member_dir).unwrap();
        fs::write(temp_dir.path().join("Cargo.toml"), root).unwrap();
        let member_manifest = member_dir.join("Cargo.toml");
        fs::write(&member_manifest, member).unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_manifest, Path::new("crates/member/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        let member_project = projects
            .iter()
            .find(|project| project.name() == Some("member"))
            .expect("the member must be discovered");
        let mut names: Vec<String> = member_project.dependencies().iter().cloned().collect();
        names.sort_unstable();

        temp_dir.close().unwrap();
        names
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_workspace_dependencies_from_all_cargo_sections()
     {
        // Every Cargo dependency table — plain, dev, build, and their
        // target-specific forms — is scanned. The dev entries carry a version
        // here so that this test isolates the "which sections are read?"
        // question from the "does the entry survive packaging?" one covered by
        // `test_rust_project_finder_omits_versionless_dev_dependencies`.
        let deps = dependency_names_of(
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
runtime-core = { workspace = true }
external = "1.0"

[dev-dependencies]
test-support = { path = "../test-support", version = "1.0" }
tempfile = "3"

[build-dependencies]
build-helper = { workspace = true }
cc = "1"

[target.'cfg(unix)'.dependencies]
unix-support = { workspace = true }
libc = "0.2"

[target.'cfg(windows)'.dev-dependencies]
windows-test-support = { path = "../windows-test-support", version = "1.0" }

[target.'cfg(target_arch = "wasm32")'.build-dependencies]
wasm-build-helper = { workspace = true }
"#,
        )
        .await;

        assert_eq!(
            deps,
            vec![
                "build-helper",
                "runtime-core",
                "test-support",
                "unix-support",
                "wasm-build-helper",
                "windows-test-support",
            ]
        );
    }

    /// `cargo package` keeps only those dev-dependencies that carry a version
    /// requirement, so a path-only one is absent from the published manifest
    /// and cannot constrain publish order. Collecting it anyway is what made
    /// this repository's own `utils --dev--> node` and `cli --dev--> cli`
    /// entries look like release-graph cycles and abort `changepacks publish`.
    #[tokio::test]
    async fn test_rust_project_finder_omits_versionless_dev_dependencies() {
        let deps = dependency_names_of(
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
runtime-core = { path = "../runtime-core", version = "1.0" }

[dev-dependencies]
dev-only = { path = "../dev-only" }
test-package = { path = ".", features = ["test-support"] }

[target.'cfg(windows)'.dev-dependencies]
windows-dev-only = { path = "../windows-dev-only" }
"#,
        )
        .await;

        assert_eq!(deps, vec!["runtime-core"]);
    }

    /// The version rule applies to dev tables ONLY. A path-only entry in
    /// `[dependencies]` / `[build-dependencies]` makes the package
    /// unpublishable outright — that is Cargo's error to report, not a reason
    /// for changepacks to forget the edge.
    #[tokio::test]
    async fn test_rust_project_finder_keeps_versionless_path_dependencies_in_published_tables() {
        let deps = dependency_names_of(
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
runtime-core = { path = "../runtime-core" }

[build-dependencies]
build-helper = { path = "../build-helper" }

[target.'cfg(unix)'.dependencies]
unix-support = { path = "../unix-support" }

[target.'cfg(unix)'.build-dependencies]
unix-build-helper = { path = "../unix-build-helper" }
"#,
        )
        .await;

        assert_eq!(
            deps,
            vec![
                "build-helper",
                "runtime-core",
                "unix-build-helper",
                "unix-support",
            ]
        );
    }

    /// An aliased dev-dependency (`alias = { package = "real" }`) still binds
    /// the edge to the real package name once it clears the version gate.
    #[tokio::test]
    async fn test_rust_project_finder_versioned_dev_dependency_resolves_package_alias() {
        let deps = dependency_names_of(
            r#"[package]
name = "test-package"
version = "1.0.0"

[dev-dependencies]
alias = { path = "../real", version = "1.0", package = "real-name" }
versionless-alias = { path = "../other", package = "other-name" }
"#,
        )
        .await;

        assert_eq!(deps, vec!["real-name"]);
    }

    /// For `dep.workspace = true` the version requirement lives on the ROOT
    /// entry, so that is what decides whether a dev edge survives packaging.
    /// Covers all three root shapes: table-with-version, scalar shorthand
    /// (`dep = "1"`, which is version-bearing despite having no `version` key),
    /// and path-only.
    #[tokio::test]
    async fn test_rust_project_finder_dev_workspace_inheritance_follows_the_root_version() {
        let deps = member_dependency_names_of(
            r#"[workspace]
members = ["crates/member"]

[workspace.package]
version = "1.0.0"

[workspace.dependencies]
versioned-dep = { path = "crates/versioned-dep", version = "1.0" }
scalar-dep = "1"
pathonly-dep = { path = "crates/pathonly-dep" }
"#,
            r#"[package]
name = "member"
version = "0.1.0"

[dev-dependencies]
versioned-dep = { workspace = true }
scalar-dep = { workspace = true }
pathonly-dep = { workspace = true }
"#,
        )
        .await;

        assert_eq!(deps, vec!["scalar-dep", "versioned-dep"]);
    }

    /// The same inherited dev entry stays an edge when it sits in a PUBLISHED
    /// table, because those survive packaging regardless of the root shape.
    #[tokio::test]
    async fn test_rust_project_finder_published_workspace_inheritance_ignores_the_root_version() {
        let deps = member_dependency_names_of(
            r#"[workspace]
members = ["crates/member"]

[workspace.package]
version = "1.0.0"

[workspace.dependencies]
pathonly-dep = { path = "crates/pathonly-dep" }
"#,
            r#"[package]
name = "member"
version = "0.1.0"

[dependencies]
pathonly-dep = { workspace = true }
"#,
        )
        .await;

        assert_eq!(deps, vec!["pathonly-dep"]);
    }

    /// A hybrid root (`[workspace]` + `[package]`) inherits from its OWN
    /// `[workspace.dependencies]`, so its dev edges must be judged against
    /// those — never against an enclosing workspace that happens to declare the
    /// same key with a version.
    #[tokio::test]
    async fn test_rust_project_finder_hybrid_root_uses_its_own_workspace_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let inner_dir = temp_dir.path().join("inner");
        fs::create_dir_all(&inner_dir).unwrap();
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["inner"]

[workspace.package]
version = "9.9.9"

[workspace.dependencies]
shared = { path = "shared", version = "1.0" }
"#,
        )
        .unwrap();
        let inner_manifest = inner_dir.join("Cargo.toml");
        fs::write(
            &inner_manifest,
            r#"[package]
name = "inner"
version = "0.1.0"

[workspace]
members = []

[workspace.package]
version = "0.1.0"

[workspace.dependencies]
shared = { path = "../shared" }

[dev-dependencies]
shared = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&temp_dir.path().join("Cargo.toml"), Path::new("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&inner_manifest, Path::new("inner/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        let inner = projects
            .iter()
            .find(|project| project.name() == Some("inner"))
            .expect("the hybrid root must be discovered");
        assert!(
            inner.dependencies().is_empty(),
            "the parent's versioned `shared` must not rescue the hybrid root's \
             path-only one, got {:?}",
            inner.dependencies()
        );

        temp_dir.close().unwrap();
    }

    /// Version-bearing classifications are per-root: a sibling workspace that
    /// declares the same dependency key with a version must not make another
    /// workspace's path-only entry look publishable.
    #[tokio::test]
    async fn test_rust_project_finder_sibling_workspaces_do_not_share_versioned_classifications() {
        let temp_dir = TempDir::new().unwrap();
        let mut finder = RustProjectFinder::new();
        for (workspace, root_shared) in [
            (
                "alpha",
                r#"shared = { path = "../shared", version = "1.0" }"#,
            ),
            ("beta", r#"shared = { path = "../shared" }"#),
        ] {
            let member_dir = temp_dir.path().join(workspace).join("member");
            fs::create_dir_all(&member_dir).unwrap();
            fs::write(
                temp_dir.path().join(workspace).join("Cargo.toml"),
                format!(
                    "[workspace]\nmembers = [\"member\"]\n\n[workspace.package]\nversion = \"1.0.0\"\n\n[workspace.dependencies]\n{root_shared}\n"
                ),
            )
            .unwrap();
            fs::write(
                member_dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{workspace}-member\"\nversion = \"0.1.0\"\n\n[dev-dependencies]\nshared = {{ workspace = true }}\n"
                ),
            )
            .unwrap();
            finder
                .visit(
                    &member_dir.join("Cargo.toml"),
                    Path::new(&format!("{workspace}/member/Cargo.toml")),
                )
                .await
                .unwrap();
        }
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        let deps_of = |name: &str| {
            projects
                .iter()
                .find(|project| project.name() == Some(name))
                .unwrap_or_else(|| panic!("{name} must be discovered"))
                .dependencies()
                .clone()
        };
        assert!(deps_of("alpha-member").contains("shared"));
        assert!(deps_of("beta-member").is_empty());

        temp_dir.close().unwrap();
    }

    /// The exact shape that aborted this repository's own release: `utils` and
    /// `cli` reach `node` / themselves only through path-only dev-dependencies,
    /// while `node --> utils` is a real `[dependencies]` edge. Only the real
    /// edge may survive, otherwise the publish batch is a cycle.
    #[tokio::test]
    async fn test_rust_project_finder_reproduces_the_changepacks_release_graph() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.3.0"

[workspace.dependencies]
changepacks-node = { path = "crates/node", version = "^0.3.0" }
changepacks-utils = { path = "crates/utils", version = "^0.3.0" }
"#,
        )
        .unwrap();
        let manifests = [
            (
                "utils",
                r#"[package]
name = "changepacks-utils"
version = "0.3.0"

[dev-dependencies]
changepacks-node = { path = "../node" }
"#,
            ),
            (
                "node",
                r#"[package]
name = "changepacks-node"
version = "0.3.0"

[dependencies]
changepacks-utils.workspace = true
"#,
            ),
            (
                "cli",
                r#"[package]
name = "changepacks-cli"
version = "0.3.0"

[dependencies]
changepacks-node.workspace = true
changepacks-utils.workspace = true

[dev-dependencies]
changepacks-cli = { path = ".", features = ["test-support"] }
"#,
            ),
        ];
        let mut finder = RustProjectFinder::new();
        for (crate_name, manifest) in manifests {
            let crate_dir = temp_dir.path().join("crates").join(crate_name);
            fs::create_dir_all(&crate_dir).unwrap();
            fs::write(crate_dir.join("Cargo.toml"), manifest).unwrap();
            finder
                .visit(
                    &crate_dir.join("Cargo.toml"),
                    Path::new(&format!("crates/{crate_name}/Cargo.toml")),
                )
                .await
                .unwrap();
        }
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        let deps_of = |name: &str| {
            let mut names: Vec<String> = projects
                .iter()
                .find(|project| project.name() == Some(name))
                .unwrap_or_else(|| panic!("{name} must be discovered"))
                .dependencies()
                .iter()
                .cloned()
                .collect();
            names.sort_unstable();
            names
        };
        assert!(
            deps_of("changepacks-utils").is_empty(),
            "the path-only dev edge to node must be gone"
        );
        assert_eq!(deps_of("changepacks-node"), vec!["changepacks-utils"]);
        assert_eq!(
            deps_of("changepacks-cli"),
            vec!["changepacks-node", "changepacks-utils"],
            "the self dev edge must be gone"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_isolates_sibling_workspaces_during_interleaved_visits() {
        // Given: sibling workspaces with distinct inherited versions
        let temp_dir = TempDir::new().unwrap();
        let (alpha_workspace, alpha_package) = write_inherited_version_workspace(
            &temp_dir.path().join("alpha"),
            "alpha-package",
            "1.2.3",
        );
        let (beta_workspace, beta_package) = write_inherited_version_workspace(
            &temp_dir.path().join("beta"),
            "beta-package",
            "4.5.6",
        );

        // When: member and root visits are interleaved across the workspaces
        let mut finder = RustProjectFinder::new();
        finder
            .visit(
                &alpha_package,
                Path::new("alpha/crates/alpha-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder
            .visit(&beta_workspace, Path::new("beta/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(
                &beta_package,
                Path::new("beta/crates/beta-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder
            .visit(&alpha_workspace, Path::new("alpha/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        // Then: each member inherits only from its containing workspace
        let projects = finder.projects();
        for (name, version, workspace_root) in [
            ("alpha-package", "1.2.3", &alpha_workspace),
            ("beta-package", "4.5.6", &beta_workspace),
        ] {
            let package = projects
                .iter()
                .copied()
                .find(|project| project.name() == Some(name))
                .unwrap();
            assert_eq!(package.version(), Some(version));
            assert_eq!(
                package.expect_package().workspace_root_path(),
                Some(workspace_root.as_path())
            );
        }
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_discovers_each_unvisited_sibling_workspace() {
        // Given: inherited-version members in sibling workspaces whose roots are ignored
        let temp_dir = TempDir::new().unwrap();
        let (alpha_workspace, alpha_package) = write_inherited_version_workspace(
            &temp_dir.path().join("alpha"),
            "alpha-package",
            "1.2.3",
        );
        let (beta_workspace, beta_package) = write_inherited_version_workspace(
            &temp_dir.path().join("beta"),
            "beta-package",
            "4.5.6",
        );

        // When: only the members are visited before finalization
        let mut finder = RustProjectFinder::new();
        finder
            .visit(
                &alpha_package,
                Path::new("alpha/crates/alpha-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder
            .visit(
                &beta_package,
                Path::new("beta/crates/beta-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        // Then: finalization discovers and applies each member's own workspace root
        let projects = finder.projects();
        for (name, version, workspace_root) in [
            ("alpha-package", "1.2.3", &alpha_workspace),
            ("beta-package", "4.5.6", &beta_workspace),
        ] {
            let package = projects
                .iter()
                .copied()
                .find(|project| project.name() == Some(name))
                .unwrap();
            assert_eq!(package.version(), Some(version));
            assert_eq!(
                package.expect_package().workspace_root_path(),
                Some(workspace_root.as_path())
            );
        }
    }

    #[tokio::test]
    async fn test_rust_project_finder_virtual_workspace_with_workspace_version() {
        // Reproduces vespera-style virtual workspace (no [package] section)
        let temp_dir = TempDir::new().unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"
"#,
        )
        .unwrap();

        let pkg_dir = temp_dir.path().join("crates").join("vespera");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_toml = pkg_dir.join("Cargo.toml");
        fs::write(
            &pkg_toml,
            r#"[package]
name = "vespera"
version.workspace = true
edition.workspace = true

[dependencies]
vespera_core = { workspace = true }

[lints]
workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/vespera/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        // Virtual workspace (no [package]) + 1 member
        assert_eq!(projects.len(), 2);

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("vespera"))
            .unwrap();
        assert_eq!(pkg.version(), Some("0.1.33"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_workspace_version() {
        let temp_dir = TempDir::new().unwrap();

        // Create workspace root
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "2.5.0"
edition = "2024"

[package]
name = "my-workspace"
version = "2.5.0"
"#,
        )
        .unwrap();

        // Create member package with version.workspace = true
        let pkg_dir = temp_dir.path().join("crates").join("my-crate");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_toml = pkg_dir.join("Cargo.toml");
        fs::write(
            &pkg_toml,
            r#"[package]
name = "my-crate"
version.workspace = true
edition.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Visit workspace first (normal git index order)
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/my-crate/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        // Find the package
        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("my-crate"))
            .unwrap();
        assert_eq!(pkg.version(), Some("2.5.0")); // Should inherit workspace version
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_before_workspace() {
        let temp_dir = TempDir::new().unwrap();

        // Create workspace root
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "3.0.0"

[package]
name = "my-workspace"
version = "3.0.0"
"#,
        )
        .unwrap();

        // Create member package
        let pkg_dir = temp_dir.path().join("crates").join("my-crate");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_toml = pkg_dir.join("Cargo.toml");
        fs::write(
            &pkg_toml,
            r#"[package]
name = "my-crate"
version.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Visit package BEFORE workspace (reverse order)
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/my-crate/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("my-crate"))
            .unwrap();
        assert_eq!(pkg.version(), Some("3.0.0")); // Should still resolve correctly
    }

    #[tokio::test]
    async fn test_rust_project_finder_workspace_ignored_by_config() {
        // Simulates when ignore patterns like ["**", "!crates/**"] skip the root Cargo.toml
        let temp_dir = TempDir::new().unwrap();

        // Create workspace root (won't be visited due to ignore)
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"
"#,
        )
        .unwrap();

        // Create 2 member packages
        for name in ["vespera", "vespera_core"] {
            let pkg_dir = temp_dir.path().join("crates").join(name);
            fs::create_dir_all(&pkg_dir).unwrap();
            fs::write(
                pkg_dir.join("Cargo.toml"),
                format!(
                    r#"[package]
name = "{name}"
version.workspace = true
edition.workspace = true

[lints]
workspace = true
"#
                ),
            )
            .unwrap();
        }

        let mut finder = RustProjectFinder::new();
        // Only visit member packages (workspace root is ignored)
        for name in ["vespera", "vespera_core"] {
            let pkg_toml = temp_dir.path().join("crates").join(name).join("Cargo.toml");
            finder
                .visit(
                    &pkg_toml,
                    &PathBuf::from(format!("crates/{name}/Cargo.toml")),
                )
                .await
                .unwrap();
        }
        // finalize should discover the workspace root by walking up
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        // 2 member packages + 1 synthetic workspace
        assert_eq!(projects.len(), 3);

        for name in ["vespera", "vespera_core"] {
            let pkg = projects.iter().find(|p| p.name() == Some(name)).unwrap();
            assert_eq!(
                pkg.version(),
                Some("0.1.33"),
                "{name} should inherit workspace version"
            );
        }

        // Synthetic workspace should exist with the workspace version
        let ws = projects
            .iter()
            .find(|p| matches!(p, Project::Workspace(_)))
            .expect("synthetic workspace should be created");
        assert_eq!(ws.version(), Some("0.1.33"));
        assert_eq!(ws.relative_path(), Path::new("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_discovers_workspace_with_package_section() {
        // When finalize() walks up to discover the workspace root, and that root
        // has a [package] section with name and version, lines 162-163 return Some(...)
        let temp_dir = TempDir::new().unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.2.0"

[package]
name = "my-workspace-root"
version = "0.2.0"
"#,
        )
        .unwrap();

        let pkg_dir = temp_dir.path().join("crates").join("my-crate");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(
            pkg_dir.join("Cargo.toml"),
            r#"[package]
name = "my-crate"
version.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Only visit member (workspace root is NOT visited — simulates ignore config)
        let pkg_toml = pkg_dir.join("Cargo.toml");
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/my-crate/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        let ws = projects
            .iter()
            .find(|p| matches!(p, Project::Workspace(_)))
            .unwrap();
        assert_eq!(ws.name(), Some("my-workspace-root"));
        assert_eq!(ws.version(), Some("0.2.0"));

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("my-crate"))
            .unwrap();
        assert_eq!(pkg.version(), Some("0.2.0"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_resolves_sibling_members_through_rejected_ancestor()
    {
        // Two `version.workspace = true` members share an ancestor chain that
        // contains one rejected candidate (`crates/Cargo.toml`: it HAS a
        // `[workspace]` table but no `[workspace.package].version`) before the
        // real root. `finalize`'s rejected-candidate memo skips the second read
        // of that ancestor; this test pins the observable contract the memo must
        // preserve — both siblings still resolve to the SAME discovered workspace
        // root and both still carry the inherited version.
        let temp_dir = TempDir::new().unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*/*"]

[workspace.package]
version = "0.3.0"
"#,
        )
        .unwrap();

        // Rejected intermediate ancestor: `[workspace]` present, but no
        // `[workspace.package].version`, so the walk must `continue` past it
        // rather than adopt it or stop.
        let intermediate_dir = temp_dir.path().join("crates");
        fs::create_dir_all(&intermediate_dir).unwrap();
        fs::write(
            intermediate_dir.join("Cargo.toml"),
            r"[workspace]
members = []
",
        )
        .unwrap();

        let mut member_tomls = Vec::new();
        for name in ["app", "cli"] {
            let member_dir = intermediate_dir.join(name).join("inner");
            fs::create_dir_all(&member_dir).unwrap();
            let member_toml = member_dir.join("Cargo.toml");
            fs::write(
                &member_toml,
                format!("[package]\nname = \"{name}\"\nversion.workspace = true\n"),
            )
            .unwrap();
            member_tomls.push((name, member_toml));
        }

        // The real root is never visited (simulates an ignore pattern), so
        // `finalize` must discover it by walking up from each member.
        let mut finder = RustProjectFinder::new();
        for (name, member_toml) in &member_tomls {
            finder
                .visit(
                    member_toml,
                    Path::new(&format!("crates/{name}/inner/Cargo.toml")),
                )
                .await
                .unwrap();
        }
        finder.finalize().await.unwrap();

        // Exactly one workspace root is discovered, and it is the real root —
        // not the rejected intermediate ancestor.
        assert_eq!(
            finder.workspace_package_versions.len(),
            1,
            "only the root carrying [workspace.package].version may be recorded"
        );
        assert_eq!(
            finder.workspace_package_versions.get(&workspace_toml),
            Some(&"0.3.0".to_string())
        );

        let projects = finder.projects();
        assert_eq!(projects.len(), 3, "two members plus one synthetic root");

        let workspaces = projects
            .iter()
            .filter(|project| matches!(project, Project::Workspace(_)))
            .collect::<Vec<_>>();
        assert_eq!(
            workspaces.len(),
            1,
            "both siblings must share one discovered workspace root"
        );
        assert_eq!(workspaces[0].path(), workspace_toml.as_path());
        assert_eq!(workspaces[0].version(), Some("0.3.0"));

        for name in ["app", "cli"] {
            let member = projects
                .iter()
                .find(|project| project.name() == Some(name))
                .unwrap_or_else(|| panic!("{name} should be discovered"));
            assert_eq!(
                member.version(),
                Some("0.3.0"),
                "{name} should inherit the workspace version"
            );
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_ignores_cargo_toml_above_git_root() {
        // Regression: when the workspace root is NOT visited (e.g. excluded by
        // ignore patterns), finalize() walks up from the first pending member
        // looking for a `Cargo.toml` carrying `[workspace.package].version`.
        // That walk must be BOUNDED to the git root — it must never climb past
        // the repository root and adopt an out-of-repo `Cargo.toml` (e.g. a
        // parent Rust project this repo is nested inside), which would silently
        // rewrite inherited-version resolution for every member. Mirrors the
        // C# finder's `test_visit_package_ignores_sln_above_repo_root`.
        let temp_dir = TempDir::new().unwrap();

        // Decoy workspace root ABOVE the simulated repo root. If the walk were
        // unbounded it would climb here and adopt version "9.9.9".
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["repo/crates/*"]

[workspace.package]
version = "9.9.9"
edition = "2024"
"#,
        )
        .unwrap();

        // Simulated repo root at <temp>/repo — deliberately has NO Cargo.toml,
        // so the bounded walk finds nothing in-repo and must stop at the root
        // instead of escaping to the decoy above it. The member sits two levels
        // below, and its relative path (3 components) pins the git root to
        // <temp>/repo via the `ancestors().nth(components)` derivation.
        let member_dir = temp_dir.path().join("repo").join("crates").join("mycrate");
        fs::create_dir_all(&member_dir).unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "mycrate"
version.workspace = true
edition.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, &PathBuf::from("crates/mycrate/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        // No synthetic workspace is adopted from the out-of-repo decoy, so the
        // member stays the only project.
        assert_eq!(
            projects.len(),
            1,
            "a Cargo.toml above the git root must not be adopted as the workspace"
        );
        assert!(
            !projects.iter().any(|p| matches!(p, Project::Workspace(_))),
            "no synthetic workspace should be created from an out-of-repo Cargo.toml"
        );

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("mycrate"))
            .expect("member package should exist");
        assert_ne!(
            pkg.version(),
            Some("9.9.9"),
            "member must not inherit the decoy workspace version from above the repo root"
        );
        assert_eq!(
            pkg.version(),
            None,
            "with no in-repo workspace root found, the member version stays unresolved"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_reports_malformed_ancestor_cargo_toml() {
        let temp_dir = TempDir::new().unwrap();
        let repository_root = temp_dir.path().join("repo");
        let member_dir = repository_root.join("crates").join("app");
        fs::create_dir_all(&member_dir).unwrap();

        let malformed_ancestor = repository_root.join("crates").join("Cargo.toml");
        fs::write(&malformed_ancestor, "invalid toml [[[").unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        let error = finder
            .visit(&member_toml, Path::new("crates/app/Cargo.toml"))
            .await
            .expect_err("a malformed ancestor manifest must fail alias discovery");
        let message = error.to_string();
        assert!(message.contains("Failed to parse Cargo.toml"));
        assert!(message.contains(malformed_ancestor.to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_reports_ancestor_cargo_toml_read_failure() {
        let temp_dir = TempDir::new().unwrap();
        let repository_root = temp_dir.path().join("repo");
        let member_dir = repository_root.join("crates").join("app");
        fs::create_dir_all(&member_dir).unwrap();

        let unreadable_ancestor = repository_root.join("crates").join("Cargo.toml");
        fs::create_dir(&unreadable_ancestor).unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        let error = finder
            .visit(&member_toml, Path::new("crates/app/Cargo.toml"))
            .await
            .expect_err("an unreadable ancestor candidate must fail alias discovery");
        let message = error.to_string();
        assert!(message.contains("Failed to read Cargo.toml"));
        assert!(message.contains(unreadable_ancestor.to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn test_rust_project_finder_uses_nearest_valid_nested_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let nested_root = temp_dir.path().join("nested");
        let member_dir = nested_root.join("crates").join("app");
        fs::create_dir_all(&member_dir).unwrap();

        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["nested"]

[workspace.dependencies]
shared = { package = "outer-core", path = "outer-core" }
"#,
        )
        .unwrap();
        fs::write(
            nested_root.join("Cargo.toml"),
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
shared = { package = "nested-core", path = "crates/core" }
"#,
        )
        .unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
shared = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("nested/crates/app/Cargo.toml"))
            .await
            .unwrap();

        let app = finder
            .projects()
            .into_iter()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert!(app.dependencies().contains("nested-core"));
        assert!(!app.dependencies().contains("outer-core"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_reuses_non_workspace_manifest_cache_for_siblings() {
        // Given: sibling members below a valid non-workspace manifest and a workspace root
        let temp_dir = TempDir::new().unwrap();
        let intermediate = temp_dir.path().join("tools");
        let app_dir = intermediate.join("crates/app");
        let cli_dir = intermediate.join("crates/cli");
        fs::create_dir_all(&app_dir).unwrap();
        fs::create_dir_all(&cli_dir).unwrap();

        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["tools/crates/*"]

[workspace.dependencies]
shared = { package = "core", path = "core" }
"#,
        )
        .unwrap();
        let intermediate_toml = intermediate.join("Cargo.toml");
        fs::write(
            &intermediate_toml,
            "[package]\nname = \"tools\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        let app_toml = app_dir.join("Cargo.toml");
        let cli_toml = cli_dir.join("Cargo.toml");
        for (name, manifest) in [("app", &app_toml), ("cli", &cli_toml)] {
            fs::write(
                manifest,
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"1.0.0\"\n\n[dependencies]\nshared = {{ workspace = true }}\n"
                ),
            )
            .unwrap();
        }

        // When: the first visit seeds the negative cache before the second sibling is visited
        let mut finder = RustProjectFinder::new();
        finder
            .visit(&app_toml, Path::new("tools/crates/app/Cargo.toml"))
            .await
            .unwrap();
        let missing_candidate = intermediate.join("crates/Cargo.toml");
        assert!(
            finder
                .non_workspace_manifest_candidates
                .contains(&missing_candidate)
        );
        assert!(
            finder
                .non_workspace_manifest_candidates
                .contains(&intermediate_toml)
        );
        fs::write(&intermediate_toml, "invalid toml [[[").unwrap();
        finder
            .visit(&cli_toml, Path::new("tools/crates/cli/Cargo.toml"))
            .await
            .unwrap();

        // Then: both sibling aliases resolve through the cached workspace root
        for name in ["app", "cli"] {
            let project = finder
                .projects()
                .into_iter()
                .find(|project| project.name() == Some(name))
                .unwrap();
            assert!(project.dependencies().contains("core"));
            assert!(!project.dependencies().contains("shared"));
        }
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_reports_malformed_ancestor_cargo_toml() {
        let temp_dir = TempDir::new().unwrap();
        let repository_root = temp_dir.path().join("repo");
        let member_dir = repository_root.join("crates").join("app");
        fs::create_dir_all(&member_dir).unwrap();

        let ancestor_manifest = repository_root.join("crates").join("Cargo.toml");
        fs::write(
            &ancestor_manifest,
            r#"[package]
name = "container"
version = "1.0.0"
"#,
        )
        .unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("crates/app/Cargo.toml"))
            .await
            .unwrap();

        fs::write(&ancestor_manifest, "invalid toml [[[").unwrap();
        let error = finder
            .finalize()
            .await
            .expect_err("a malformed ancestor manifest must fail version discovery");
        let message = error.to_string();
        assert!(message.contains("Failed to parse Cargo.toml"));
        assert!(message.contains(ancestor_manifest.to_string_lossy().as_ref()));
        assert!(finder.projects().is_empty());
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_reports_ancestor_cargo_toml_read_failure() {
        let temp_dir = TempDir::new().unwrap();
        let repository_root = temp_dir.path().join("repo");
        let member_dir = repository_root.join("crates").join("app");
        fs::create_dir_all(&member_dir).unwrap();

        let ancestor_manifest = repository_root.join("crates").join("Cargo.toml");
        fs::write(
            &ancestor_manifest,
            r#"[package]
name = "container"
version = "1.0.0"
"#,
        )
        .unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("crates/app/Cargo.toml"))
            .await
            .unwrap();

        fs::remove_file(&ancestor_manifest).unwrap();
        fs::create_dir(&ancestor_manifest).unwrap();
        let error = finder
            .finalize()
            .await
            .expect_err("an unreadable ancestor candidate must fail version discovery");
        let message = error.to_string();
        assert!(message.contains("Failed to read Cargo.toml"));
        assert!(message.contains(ancestor_manifest.to_string_lossy().as_ref()));
        assert!(finder.projects().is_empty());
    }

    #[tokio::test]
    async fn test_workspace_version_bump_only_fans_out_to_inheriting_members() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "1.2.3"

[workspace.dependencies]
inline-inherited = { path = "crates/inline-inherited", version = "1.2.3" }
fixed-member = { path = "crates/fixed-member", version = "1.2.3" } # fixed bytes stay unchanged

[workspace.dependencies.subtable-inherited]
path = "crates/subtable-inherited"
version = "1.2.3"
features = ["derive"]
"#,
        )
        .unwrap();

        let members = [
            ("inline-inherited", "version.workspace = true"),
            ("subtable-inherited", "version = { workspace = true }"),
            ("fixed-member", "version = \"1.2.3\""),
        ];
        let mut member_tomls = Vec::new();
        for (name, version) in members {
            let member_dir = temp_dir.path().join("crates").join(name);
            fs::create_dir_all(&member_dir).unwrap();
            let member_toml = member_dir.join("Cargo.toml");
            fs::write(
                &member_toml,
                format!("[package]\nname = \"{name}\"\n{version}\n"),
            )
            .unwrap();
            member_tomls.push((name, member_toml));
        }

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&workspace_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();
        for (name, member_toml) in &member_tomls {
            finder
                .visit(member_toml, Path::new(&format!("crates/{name}/Cargo.toml")))
                .await
                .unwrap();
        }
        finder.finalize().await.unwrap();

        let workspace = finder
            .projects_mut()
            .into_iter()
            .find(|project| matches!(project, Project::Workspace(_)))
            .expect("workspace project should be discovered");
        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&workspace_toml).unwrap();
        let document: toml_edit::DocumentMut = content.parse().unwrap();
        let dependencies = document["workspace"]["dependencies"].as_table().unwrap();
        assert_eq!(
            dependencies["inline-inherited"]["version"].as_str(),
            Some("1.2.4")
        );
        assert_eq!(
            dependencies["subtable-inherited"]["version"].as_str(),
            Some("1.2.4")
        );
        assert_eq!(
            dependencies["subtable-inherited"]["features"]
                .as_array()
                .map(toml_edit::Array::len),
            Some(1)
        );
        assert_eq!(
            dependencies["fixed-member"]["version"].as_str(),
            Some("1.2.3")
        );
        assert!(content.contains(
            "fixed-member = { path = \"crates/fixed-member\", version = \"1.2.3\" } # fixed bytes stay unchanged"
        ));
    }

    #[tokio::test]
    async fn test_workspace_version_bump_fanout_when_member_is_visited_first() {
        let temp_dir = TempDir::new().unwrap();
        let (workspace_toml, member_toml) =
            write_inherited_version_fanout_workspace(temp_dir.path(), "member-first", "1.0.0");
        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("crates/member-first/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&workspace_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        bump_workspace(&mut finder, Path::new("Cargo.toml")).await;

        let content = fs::read_to_string(&workspace_toml).unwrap();
        let document: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(
            document["workspace"]["dependencies"]["member-first"]["version"].as_str(),
            Some("1.0.1")
        );
    }

    #[tokio::test]
    async fn test_workspace_version_bump_fanout_when_root_is_visited_first() {
        let temp_dir = TempDir::new().unwrap();
        let (workspace_toml, member_toml) =
            write_inherited_version_fanout_workspace(temp_dir.path(), "root-first", "1.1.0");
        let mut finder = RustProjectFinder::new();
        finder
            .visit(&workspace_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&member_toml, Path::new("crates/root-first/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        bump_workspace(&mut finder, Path::new("Cargo.toml")).await;

        let content = fs::read_to_string(&workspace_toml).unwrap();
        let document: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(
            document["workspace"]["dependencies"]["root-first"]["version"].as_str(),
            Some("1.1.1")
        );
    }

    #[tokio::test]
    async fn test_workspace_version_bump_fanout_for_synthetic_ignored_root() {
        let temp_dir = TempDir::new().unwrap();
        let (workspace_toml, member_toml) =
            write_inherited_version_fanout_workspace(temp_dir.path(), "synthetic-member", "1.2.0");
        let mut finder = RustProjectFinder::new();
        finder
            .visit(
                &member_toml,
                Path::new("crates/synthetic-member/Cargo.toml"),
            )
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        bump_workspace(&mut finder, Path::new("Cargo.toml")).await;

        let content = fs::read_to_string(&workspace_toml).unwrap();
        let document: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(
            document["workspace"]["dependencies"]["synthetic-member"]["version"].as_str(),
            Some("1.2.1")
        );
    }

    #[tokio::test]
    async fn test_workspace_version_bump_fanout_uses_nearest_interleaved_nested_root() {
        let temp_dir = TempDir::new().unwrap();
        let (outer_workspace, outer_member) =
            write_inherited_version_fanout_workspace(temp_dir.path(), "outer-member", "3.0.0");
        let nested_root = temp_dir.path().join("nested");
        let (nested_workspace, nested_member) =
            write_inherited_version_fanout_workspace(&nested_root, "nested-member", "4.0.0");

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&outer_workspace, Path::new("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(
                &nested_member,
                Path::new("nested/crates/nested-member/Cargo.toml"),
            )
            .await
            .unwrap();
        finder
            .visit(&outer_member, Path::new("crates/outer-member/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&nested_workspace, Path::new("nested/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        bump_workspace(&mut finder, Path::new("Cargo.toml")).await;
        bump_workspace(&mut finder, Path::new("nested/Cargo.toml")).await;

        let outer_content = fs::read_to_string(&outer_workspace).unwrap();
        let outer_document: toml_edit::DocumentMut = outer_content.parse().unwrap();
        assert_eq!(
            outer_document["workspace"]["dependencies"]["outer-member"]["version"].as_str(),
            Some("3.0.1")
        );
        let nested_content = fs::read_to_string(&nested_workspace).unwrap();
        let nested_document: toml_edit::DocumentMut = nested_content.parse().unwrap();
        assert_eq!(
            nested_document["workspace"]["dependencies"]["nested-member"]["version"].as_str(),
            Some("4.0.1")
        );
    }

    #[tokio::test]
    async fn test_workspace_version_bump_handles_aliases_without_key_name_collisions() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "2.3.4"

[workspace.dependencies]
inline-core = { package = "core", path = "crates/core", version = "^2.3.4" }
core = { package = "fixed-real", path = "crates/fixed-real", version = "2.3.4" } # alias collision stays fixed

[workspace.dependencies.subtable-renamed]
package = "subtable-real"
path = "crates/subtable-real"
version = "~2.3.4"
"#,
        )
        .unwrap();

        let members = [
            ("core", "version.workspace = true"),
            ("subtable-real", "version = { workspace = true }"),
            ("fixed-real", "version = \"2.3.4\""),
        ];
        let mut member_tomls = Vec::new();
        for (name, version) in members {
            let member_dir = temp_dir.path().join("crates").join(name);
            fs::create_dir_all(&member_dir).unwrap();
            let member_toml = member_dir.join("Cargo.toml");
            fs::write(
                &member_toml,
                format!("[package]\nname = \"{name}\"\n{version}\n"),
            )
            .unwrap();
            member_tomls.push((name, member_toml));
        }

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&workspace_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();
        for (name, member_toml) in &member_tomls {
            finder
                .visit(member_toml, Path::new(&format!("crates/{name}/Cargo.toml")))
                .await
                .unwrap();
        }
        finder.finalize().await.unwrap();

        bump_workspace(&mut finder, Path::new("Cargo.toml")).await;

        let content = fs::read_to_string(&workspace_toml).unwrap();
        let document: toml_edit::DocumentMut = content.parse().unwrap();
        let dependencies = document["workspace"]["dependencies"].as_table().unwrap();
        assert_eq!(
            dependencies["inline-core"]["version"].as_str(),
            Some("^2.3.5")
        );
        assert_eq!(
            dependencies["subtable-renamed"]["version"].as_str(),
            Some("~2.3.5")
        );
        assert_eq!(dependencies["core"]["version"].as_str(), Some("2.3.4"));
        assert!(content.contains(
            "core = { package = \"fixed-real\", path = \"crates/fixed-real\", version = \"2.3.4\" } # alias collision stays fixed"
        ));
        assert!(content.contains("[workspace.dependencies.subtable-renamed]"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_malformed_cargo_toml() {
        // Given: a malformed Cargo.toml file
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "invalid toml [[[").unwrap();

        // When: visit is called on the malformed manifest
        let mut finder = RustProjectFinder::new();
        let result = finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await;

        // Then: the error includes both the manifest path and "Failed to parse Cargo.toml"
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to parse Cargo.toml"),
            "error message should contain 'Failed to parse Cargo.toml', got: {err_msg}"
        );
        assert!(
            err_msg.contains(cargo_toml.to_string_lossy().as_ref()),
            "error message should contain the manifest path, got: {err_msg}"
        );

        temp_dir.close().unwrap();
    }

    // A `[target]` entry whose value is NOT a table (Cargo itself would reject
    // it, but a malformed or hand-edited manifest can still reach the finder)
    // is skipped rather than treated as a dependency table. The real
    // `[dependencies]` edge still lands, so the skip is scoped to that one
    // entry.
    #[tokio::test]
    async fn test_rust_project_finder_skips_non_table_target_entry() {
        let (_temp_dir, finder) = visit_single_manifest(
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
runtime-core = { workspace = true }

[target]
"cfg(unix)" = "not-a-table"
"#,
        )
        .await;

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let deps = projects[0].expect_package().dependencies();
        assert_eq!(
            deps.len(),
            1,
            "a scalar [target] entry must contribute no edges, got {deps:?}"
        );
        assert!(deps.contains("runtime-core"));
    }

    // The alias walk starts two ancestors above the manifest, so a manifest
    // that HAS no such ancestor leaves the loop with nothing to inspect. It
    // must fall through cleanly instead of recording a root or memoizing a
    // candidate it never read.
    #[tokio::test]
    async fn test_discover_workspace_dependency_aliases_without_ancestors_records_nothing() {
        let mut finder = RustProjectFinder::new();

        finder
            .discover_workspace_dependency_aliases_for_member(
                Path::new("Cargo.toml"),
                Path::new("Cargo.toml"),
            )
            .await
            .unwrap();

        assert!(
            finder.workspace_roots.is_empty(),
            "no ancestor was inspected, so no root may be recorded"
        );
        assert!(
            finder.non_workspace_manifest_candidates.is_empty(),
            "no ancestor was inspected, so no candidate may be memoized"
        );
    }

    // Sibling of the alias walk above: `finalize`'s root search also starts two
    // ancestors up, so a manifest without one reports "no workspace root"
    // without reading — or memoizing — any candidate.
    #[tokio::test]
    async fn test_discover_workspace_root_for_member_without_ancestors_finds_nothing() {
        let finder = RustProjectFinder::new();
        let mut rejected_candidates = HashSet::new();

        let discovered = finder
            .discover_workspace_root_for_member(
                Path::new("Cargo.toml"),
                Path::new(""),
                &mut rejected_candidates,
            )
            .await
            .unwrap();

        assert!(discovered.is_none());
        assert!(
            rejected_candidates.is_empty(),
            "no candidate was read, so none may be memoized as rejected"
        );
    }

    // The alias rewrite inside `insert_workspace_member` is the LAST chance to
    // resolve a `[workspace.dependencies]` alias. Here the member's own visit
    // walks up only as far as the NESTED `[workspace]` root, which declares no
    // aliases; the outer root that owns both `[workspace.package].version` and
    // the alias is discovered later, by `finalize`. The deferred member must
    // still end up depending on the REAL package name.
    #[tokio::test]
    async fn test_rust_project_finder_renames_alias_dependency_resolved_during_finalize() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["nested/crates/*"]

[workspace.package]
version = "1.4.0"

[workspace.dependencies]
renamed-core = { package = "core", path = "nested/crates/core" }
"#,
        )
        .unwrap();

        // Nested root: a `[workspace]` table with NO `[workspace.package]`, so
        // the member's visit-time walk stops here and learns no aliases, while
        // `finalize` rejects it and keeps climbing.
        let nested_dir = temp_dir.path().join("nested");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::write(
            nested_dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();

        let member_dir = nested_dir.join("crates").join("app");
        fs::create_dir_all(&member_dir).unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version.workspace = true

[dependencies]
renamed-core = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("nested/crates/app/Cargo.toml"))
            .await
            .unwrap();

        // The visit-time rewrite could not help: the only root known so far is
        // the alias-less nested one, so the deferred member still carries the
        // raw alias key. This is what forces the rewrite into
        // `insert_workspace_member` below.
        assert_eq!(
            finder.pending_workspace_packages[0].dependencies,
            vec!["renamed-core".to_string()],
            "visit must not have resolved the alias yet"
        );

        finder.finalize().await.unwrap();

        let projects = finder.projects();
        let app = projects
            .iter()
            .find(|project| project.name() == Some("app"))
            .expect("member should be discovered");
        assert_eq!(app.version(), Some("1.4.0"));
        let dependencies = app.dependencies();
        assert!(
            dependencies.contains("core"),
            "the alias must resolve to the real package name, got {dependencies:?}"
        );
        assert!(!dependencies.contains("renamed-core"));

        temp_dir.close().unwrap();
    }

    // A workspace root discovered by `finalize` (never visited — e.g. excluded
    // by an ignore pattern) may itself be a hybrid root whose `[package]`
    // inherits `publish` from its own `[workspace.package]`. The synthetic
    // workspace project must resolve that inheritance against the very bytes
    // the walk decoded, not fall back to the permissive default.
    #[tokio::test]
    async fn test_rust_project_finder_finalize_root_resolves_own_inherited_publish() {
        let temp_dir = TempDir::new().unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.5.0"
publish = false

[package]
name = "root-crate"
version.workspace = true
publish.workspace = true
"#,
        )
        .unwrap();

        let member_dir = temp_dir.path().join("crates").join("app");
        fs::create_dir_all(&member_dir).unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            "[package]\nname = \"app\"\nversion.workspace = true\n",
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("crates/app/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        let workspace = projects
            .iter()
            .find(|project| matches!(project, Project::Workspace(_)))
            .expect("finalize should synthesize the discovered workspace root");
        assert_eq!(workspace.path(), workspace_toml.as_path());
        assert!(
            !workspace.is_publishable_by_default(),
            "publish.workspace = true must resolve to [workspace.package].publish = false"
        );

        let member = projects
            .iter()
            .find(|project| project.name() == Some("app"))
            .expect("member should be discovered");
        assert!(
            member.is_publishable_by_default(),
            "the member declares no publish key, so it stays publishable"
        );

        temp_dir.close().unwrap();
    }
}
