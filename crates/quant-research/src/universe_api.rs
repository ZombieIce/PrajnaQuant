//! Local, versioned user configuration storage for Universe HTTP endpoints.
//! The warehouse remains a separate single-writer process and is never written here.

use crate::universe::{AssetScope, ManualMember, SourceKind, UniverseDefinition};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniverseDraft {
    pub name: String,
    pub description: String,
    pub asset_scope: AssetScope,
    pub source_kind: SourceKind,
    pub source_ref: String,
    #[serde(default)]
    pub members: Vec<ManualMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniverseConfig {
    pub universe_id: Uuid,
    pub name: String,
    pub description: String,
    pub asset_scope: AssetScope,
    pub source_kind: SourceKind,
    pub source_ref: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub draft: Option<UniverseDraft>,
    pub versions: Vec<UniverseDefinition>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UniverseSummary {
    pub universe_id: Uuid,
    pub name: String,
    pub description: String,
    pub asset_scope: AssetScope,
    pub source_kind: SourceKind,
    pub source_ref: String,
    pub status: String,
    pub version_count: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishPreview {
    pub changed: bool,
    pub current_version_id: Option<Uuid>,
    pub next_version_number: usize,
    pub changed_fields: Vec<String>,
    pub draft_hash: String,
}

#[derive(Debug, Clone)]
pub struct PublishOutcome {
    pub version: UniverseDefinition,
    pub created: bool,
}

pub struct UniverseStore {
    directory: PathBuf,
    /// One process-local lock serializes every read/modify/write transaction.
    gate: Mutex<()>,
}

impl UniverseStore {
    pub fn new(output: &Path) -> Self {
        Self {
            directory: output.join("universes"),
            gate: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<UniverseConfig>> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("universe storage lock poisoned"))?;
        let mut items = Vec::new();
        if !self.directory.exists() {
            return Ok(items);
        }
        for entry in fs::read_dir(&self.directory).context("read universes directory")? {
            let path = entry?.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let item: UniverseConfig = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            items.push(item);
        }
        items.sort_by_key(|a| a.universe_id);
        Ok(items)
    }

    pub fn get(&self, id: Uuid) -> Result<UniverseConfig> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("universe storage lock poisoned"))?;
        self.load_locked(id)
    }

    pub fn create(&self, draft: UniverseDraft) -> Result<UniverseConfig> {
        validate_draft(&draft)?;
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("universe storage lock poisoned"))?;
        let now = chrono::Utc::now().to_rfc3339();
        let id = Uuid::new_v4();
        let config = UniverseConfig {
            universe_id: id,
            name: draft.name.clone(),
            description: draft.description.clone(),
            asset_scope: draft.asset_scope,
            source_kind: draft.source_kind,
            source_ref: draft.source_ref.clone(),
            status: "active".into(),
            created_at: now.clone(),
            updated_at: now,
            draft: Some(draft),
            versions: vec![],
        };
        self.save_locked(&config)?;
        Ok(config)
    }

    pub fn patch_draft(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        asset_scope: Option<AssetScope>,
        source_ref: Option<String>,
        members: Option<Vec<ManualMember>>,
    ) -> Result<UniverseConfig> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("universe storage lock poisoned"))?;
        let mut config = self.load_locked(id)?;
        ensure!(config.status != "archived", "universe is archived");
        let draft = config.draft.as_mut().context("no editable draft exists")?;
        let original = draft.clone();
        if let Some(value) = name {
            draft.name = value;
        }
        if let Some(value) = description {
            draft.description = value;
        }
        if let Some(value) = asset_scope {
            draft.asset_scope = value;
        }
        if let Some(value) = source_ref {
            draft.source_ref = value;
        }
        if let Some(value) = members {
            draft.members = value;
        }
        validate_draft(draft)?;
        if *draft == original {
            return Ok(config);
        }
        config.name = draft.name.clone();
        config.description = draft.description.clone();
        config.asset_scope = draft.asset_scope;
        config.source_ref = draft.source_ref.clone();
        config.updated_at = chrono::Utc::now().to_rfc3339();
        self.save_locked(&config)?;
        Ok(config)
    }

    pub fn preview_publish(&self, id: Uuid) -> Result<PublishPreview> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("universe storage lock poisoned"))?;
        let config = self.load_locked(id)?;
        ensure!(config.status != "archived", "universe is archived");
        let draft = config.draft.as_ref().context("no draft to publish")?;
        let previous = config.versions.last();
        let candidate =
            definition_from_draft(id, previous.map_or(Uuid::nil(), |v| v.version_id), draft)?;
        let draft_hash = candidate.calculate_content_hash()?;
        let changed = previous.is_none_or(|version| version.content_hash != draft_hash);
        let mut changed_fields = Vec::new();
        match previous {
            None => changed_fields.push("首次发布".into()),
            Some(version) => {
                if version.name != candidate.name {
                    changed_fields.push("名称".into());
                }
                if version.description != candidate.description {
                    changed_fields.push("简介".into());
                }
                if version.asset_scope != candidate.asset_scope {
                    changed_fields.push("资产范围".into());
                }
                if version.source_kind != candidate.source_kind
                    || version.source_ref != candidate.source_ref
                {
                    changed_fields.push("成员来源".into());
                }
                if version.manual_members != candidate.manual_members {
                    changed_fields.push("成员名单".into());
                }
                if version.index_events != candidate.index_events
                    || version.coverage != candidate.coverage
                {
                    changed_fields.push("历史成分数据".into());
                }
            }
        }
        Ok(PublishPreview {
            changed,
            current_version_id: previous.map(|version| version.version_id),
            next_version_number: if changed {
                config.versions.len() + 1
            } else {
                config.versions.len()
            },
            changed_fields,
            draft_hash,
        })
    }

    pub fn publish(&self, id: Uuid, expected_draft_hash: Option<&str>) -> Result<PublishOutcome> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("universe storage lock poisoned"))?;
        let mut config = self.load_locked(id)?;
        ensure!(config.status != "archived", "universe is archived");
        let draft = config.draft.as_ref().context("no draft to publish")?;
        let previous = config.versions.last();
        let comparison =
            definition_from_draft(id, previous.map_or(Uuid::nil(), |v| v.version_id), draft)?;
        let draft_hash = comparison.calculate_content_hash()?;
        if let Some(expected) = expected_draft_hash {
            ensure!(
                expected == draft_hash,
                "draft changed after publish confirmation; review the changes and try again"
            );
        }
        if let Some(version) = previous.filter(|version| version.content_hash == draft_hash) {
            return Ok(PublishOutcome {
                version: version.clone(),
                created: false,
            });
        }
        let mut definition = definition_from_draft(id, Uuid::new_v4(), draft)?;
        definition.content_hash = definition.calculate_content_hash()?;
        config.versions.push(definition.clone());
        config.updated_at = chrono::Utc::now().to_rfc3339();
        self.save_locked(&config)?;
        Ok(PublishOutcome {
            version: definition,
            created: true,
        })
    }

    pub fn delete_or_archive(&self, id: Uuid) -> Result<UniverseConfig> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("universe storage lock poisoned"))?;
        let mut config = self.load_locked(id)?;
        if config.versions.is_empty() {
            fs::remove_file(self.path(id)).context("delete universe draft")?;
            config.status = "deleted".into();
            config.draft = None;
        } else {
            config.status = "archived".into();
            config.draft = None;
            config.updated_at = chrono::Utc::now().to_rfc3339();
            self.save_locked(&config)?;
        }
        Ok(config)
    }

    fn path(&self, id: Uuid) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
    fn load_locked(&self, id: Uuid) -> Result<UniverseConfig> {
        let path = self.path(id);
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&bytes).context("parse universe config")
    }
    fn save_locked(&self, config: &UniverseConfig) -> Result<()> {
        fs::create_dir_all(&self.directory).context("create universes directory")?;
        let path = self.path(config.universe_id);
        let temp = self
            .directory
            .join(format!(".{}.{}.tmp", config.universe_id, Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(config)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        if let Err(error) = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &path)?;
            Ok(())
        })() {
            let _ = fs::remove_file(&temp);
            return Err(error).with_context(|| format!("atomically write {}", path.display()));
        }
        Ok(())
    }
}

impl From<&UniverseConfig> for UniverseSummary {
    fn from(v: &UniverseConfig) -> Self {
        Self {
            universe_id: v.universe_id,
            name: v.name.clone(),
            description: v.description.clone(),
            asset_scope: v.asset_scope,
            source_kind: v.source_kind,
            source_ref: v.source_ref.clone(),
            status: v.status.clone(),
            version_count: v.versions.len(),
            updated_at: v.updated_at.clone(),
        }
    }
}

fn definition_from_draft(
    id: Uuid,
    version_id: Uuid,
    draft: &UniverseDraft,
) -> Result<UniverseDefinition> {
    validate_draft(draft)?;
    let mut members = draft.members.clone();
    if draft.source_kind == SourceKind::IndexHistory {
        ensure!(
            members.is_empty(),
            "index_history members are sourced only from verified warehouse facts"
        );
    }
    members.sort_by(|a, b| {
        (&a.instrument.instrument_id, a.effective_from)
            .cmp(&(&b.instrument.instrument_id, b.effective_from))
    });
    for (index, member) in members.iter().enumerate() {
        for later in members.iter().skip(index + 1) {
            if member.instrument.instrument_id != later.instrument.instrument_id {
                break;
            }
            ensure!(
                member
                    .effective_to
                    .is_some_and(|end| end <= later.effective_from),
                "overlapping intervals for a member"
            );
            ensure!(
                member.instrument.exchange == later.instrument.exchange
                    && member.instrument.code == later.instrument.code
                    && member.instrument.asset_type == later.instrument.asset_type,
                "instrument identity fields conflict for one instrument_id"
            );
        }
    }
    Ok(UniverseDefinition {
        universe_id: id,
        version_id,
        name: draft.name.clone(),
        description: draft.description.clone(),
        asset_scope: draft.asset_scope,
        source_kind: draft.source_kind,
        source_ref: draft.source_ref.clone(),
        content_hash: String::new(),
        manual_members: if draft.source_kind == SourceKind::Manual {
            members
        } else {
            vec![]
        },
        // Provider not wired: never turn user input into historical membership evidence.
        index_events: vec![],
        coverage: vec![],
    })
}

pub fn validate_draft(draft: &UniverseDraft) -> Result<()> {
    ensure!(!draft.name.trim().is_empty(), "name must not be empty");
    ensure!(
        draft.name.len() <= 200 && draft.description.len() <= 4000,
        "name or description too long"
    );
    ensure!(
        !draft.source_ref.trim().is_empty(),
        "source_ref must not be empty"
    );
    if draft.source_kind == SourceKind::Manual {
        ensure!(
            draft
                .members
                .iter()
                .all(|m| !m.instrument.instrument_id.trim().is_empty()),
            "instrument_id must not be empty"
        );
        ensure!(
            draft
                .members
                .iter()
                .all(|m| m.effective_to.is_none_or(|e| m.effective_from < e)),
            "member interval must be non-empty"
        );
        let types = draft
            .members
            .iter()
            .map(|m| m.instrument.asset_type)
            .collect::<Vec<_>>();
        ensure!(
            types.is_empty()
                || (draft.asset_scope == AssetScope::Etf
                    && types.iter().all(|x| *x == crate::universe::AssetType::Etf))
                || (draft.asset_scope == AssetScope::Stock
                    && types
                        .iter()
                        .all(|x| *x == crate::universe::AssetType::Stock))
                || draft.asset_scope == AssetScope::Mixed,
            "member asset type does not match asset_scope"
        );
    }
    if draft.source_kind == SourceKind::IndexHistory {
        ensure!(
            draft.asset_scope == AssetScope::Stock,
            "index-history source must have stock asset_scope"
        );
        ensure!(
            draft.members.is_empty(),
            "index_history members are provider-managed"
        );
    }
    Ok(())
}
