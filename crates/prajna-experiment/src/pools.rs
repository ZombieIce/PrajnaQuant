//! Local research shortlists publish existing immutable StaticUniverse objects.
use fs2::FileExt;
use prajna_domain::InstrumentId;
use prajna_research::StaticUniverse;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    path::Path,
};

const PRESETS: [(&str, &str, &[&str]); 3] = [
    (
        "cross-asset",
        "跨资产 ETF",
        &[
            "513300.XSHG",
            "518880.XSHG",
            "159925.XSHE",
            "159612.XSHE",
            "159952.XSHE",
        ],
    ),
    (
        "dividend",
        "红利 ETF",
        &["510880.XSHG", "515180.XSHG", "512890.XSHG", "515100.XSHG"],
    ),
    (
        "bonds",
        "国债/政金债 ETF",
        &["511010.XSHG", "511260.XSHG", "511090.XSHG", "511520.XSHG"],
    ),
];

#[derive(Debug, Serialize)]
pub struct PoolError {
    pub code: &'static str,
    pub message: String,
}
fn error(code: &'static str, message: impl ToString) -> PoolError {
    PoolError {
        code,
        message: message.to_string(),
    }
}
fn storage(e: impl ToString) -> PoolError {
    error("pool_store_failed", e)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PoolVersion {
    pub version: usize,
    pub universe: String,
    pub members: Vec<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pool {
    pub id: String,
    pub name: String,
    pub draft: Vec<String>,
    pub versions: Vec<PoolVersion>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: u32,
    pools: Vec<Pool>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    List {},
    Save {
        id: String,
        members: Vec<String>,
        expected: Vec<String>,
    },
    Publish {
        id: String,
        expected: Vec<String>,
    },
}

fn members(values: &[String]) -> Result<Vec<String>, PoolError> {
    if values.is_empty() || values.len() > 1000 {
        return Err(error("invalid_pool", "pool requires 1–1000 members"));
    }
    let mut normalized = Vec::new();
    for value in values {
        let id: InstrumentId = value.parse().map_err(|e| error("invalid_pool", e))?;
        let symbol = value.split('.').next().unwrap_or("");
        if !matches!(id.venue().as_str(), "XSHG" | "XSHE")
            || symbol.len() != 6
            || !symbol.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(error(
                "invalid_pool",
                "six-digit XSHG/XSHE identity required",
            ));
        }
        normalized.push(id.to_string());
    }
    normalized.sort();
    if normalized.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(error("invalid_pool", "duplicate member"));
    }
    Ok(normalized)
}

fn universe(pool: &Pool, version: usize, values: &[String]) -> Result<StaticUniverse, PoolError> {
    StaticUniverse::new(
        format!("research-pool@1:{}:{version}", pool.id),
        values
            .iter()
            .map(|v| v.parse().map_err(storage))
            .collect::<Result<_, _>>()?,
    )
    .map_err(storage)
}

fn publish(root: &Path, pool: &mut Pool) -> Result<(), PoolError> {
    if pool
        .versions
        .last()
        .is_some_and(|v| v.members == pool.draft)
    {
        return Ok(());
    }
    let version = pool.versions.len() + 1;
    let frozen = universe(pool, version, &pool.draft)?;
    frozen.store(root).map_err(storage)?;
    pool.versions.push(PoolVersion {
        version,
        universe: frozen.id().map_err(storage)?,
        members: pool.draft.clone(),
    });
    Ok(())
}

fn load(root: &Path) -> Result<Registry, PoolError> {
    let path = root.join("research-pools/registry.json");
    if !path.exists() {
        let mut pools = Vec::new();
        for (id, name, values) in PRESETS {
            let mut pool = Pool {
                id: id.into(),
                name: name.into(),
                draft: members(&values.iter().map(|v| (*v).into()).collect::<Vec<_>>())?,
                versions: vec![],
            };
            publish(root, &mut pool)?;
            pools.push(pool);
        }
        return Ok(Registry {
            schema_version: 1,
            pools,
        });
    }
    let registry: Registry =
        serde_json::from_slice(&fs::read(path).map_err(storage)?).map_err(storage)?;
    if registry.schema_version != 1 || registry.pools.len() != PRESETS.len() {
        return Err(storage("unsupported pool registry"));
    }
    for (pool, (id, name, _)) in registry.pools.iter().zip(PRESETS) {
        if pool.id != id
            || pool.name != name
            || members(&pool.draft).map_err(|e| storage(e.message))? != pool.draft
            || pool.versions.is_empty()
        {
            return Err(storage("invalid pool registry"));
        }
        for (index, version) in pool.versions.iter().enumerate() {
            let expected = universe(pool, index + 1, &version.members)?;
            if version.version != index + 1
                || members(&version.members).map_err(|e| storage(e.message))? != version.members
                || expected.id().map_err(storage)? != version.universe
                || StaticUniverse::load(root, &version.universe).map_err(storage)? != expected
            {
                return Err(storage("pool version integrity mismatch"));
            }
        }
    }
    Ok(registry)
}

/// Process-shared lock and atomic replacement prevent partial/lost registry updates.
pub fn command(root: &Path, command: Command) -> Result<Value, PoolError> {
    if !root.is_dir() {
        return Err(error("invalid_dataset_root", "lake root must exist"));
    }
    let directory = root.join("research-pools");
    fs::create_dir_all(&directory).map_err(storage)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("registry.lock"))
        .map_err(storage)?;
    lock.lock_exclusive().map_err(storage)?;
    let mut registry = load(root)?;
    match command {
        Command::List {} => {}
        Command::Save {
            id,
            members: values,
            expected,
        } => {
            let pool = registry
                .pools
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| error("pool_not_found", "unknown pool"))?;
            if pool.draft != expected {
                return Err(error(
                    "pool_conflict",
                    "draft changed; reload before saving",
                ));
            }
            pool.draft = members(&values)?;
        }
        Command::Publish { id, expected } => {
            let pool = registry
                .pools
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| error("pool_not_found", "unknown pool"))?;
            if pool.draft != expected {
                return Err(error(
                    "pool_conflict",
                    "draft changed; reload before publishing",
                ));
            }
            publish(root, pool)?;
        }
    }
    let bytes = serde_json::to_vec(&registry).map_err(storage)?;
    let path = directory.join("registry.json");
    if !path.exists() || fs::read(&path).map_err(storage)? != bytes {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(&directory).map_err(storage)?;
        file.write_all(&bytes).map_err(storage)?;
        file.as_file().sync_all().map_err(storage)?;
        file.persist(path).map_err(storage)?;
    }
    Ok(json!({"schema_version":1,"data":registry}))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub id: String,
    pub version: usize,
}
pub fn resolve(root: &Path, reference: &Reference) -> Result<PoolVersion, PoolError> {
    let registry = command(root, Command::List {})?;
    let pools: Vec<Pool> =
        serde_json::from_value(registry["data"]["pools"].clone()).map_err(storage)?;
    pools
        .into_iter()
        .find(|p| p.id == reference.id)
        .and_then(|p| {
            p.versions
                .into_iter()
                .find(|v| v.version == reference.version)
        })
        .ok_or_else(|| error("pool_not_found", "published pool version not found"))
}
