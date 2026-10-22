use crate::{
    document::{Documents, path_kind, sha, validate_all},
    error::{Result, conflict, invalid},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub hash: String,
    pub response: Value,
}
#[derive(Clone, Default)]
pub struct State {
    pub documents: Documents,
    pub receipts: BTreeMap<String, Receipt>,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    paths: Vec<String>,
    receipts: BTreeMap<String, Receipt>,
}
pub struct Store {
    pub last_commit_ms: Option<f64>,
    root: PathBuf,
    pub state: State,
    pub revision: Option<String>,
    _lock: File,
}

fn plain(path: &Path) -> Result<()> {
    if let Ok(m) = fs::symlink_metadata(path)
        && m.file_type().is_symlink()
    {
        return Err(invalid("存储不接受符号链接"));
    }
    Ok(())
}
fn exclusive_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        plain(root)?;
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        let root = root.canonicalize()?;
        let lock = root.join(".writer.lock");
        plain(&lock)?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock)?;
        f.try_lock_exclusive()
            .map_err(|_| conflict("数据目录已被另一个服务或导入进程占用"))?;
        plain(&root.join("revisions"))?;
        fs::create_dir_all(root.join("revisions"))?;
        let head = root.join("HEAD");
        plain(&head)?;
        let mut state = State::default();
        let mut revision = None;
        if head.exists() {
            let id = fs::read_to_string(head)?;
            Uuid::parse_str(id.trim()).map_err(|_| invalid("存储HEAD损坏"))?;
            let dir = root.join("revisions").join(id.trim());
            plain(&dir)?;
            plain(&dir.join(".meta.json"))?;
            let manifest: Manifest = serde_json::from_slice(&fs::read(dir.join(".meta.json"))?)
                .map_err(|_| invalid("存储清单损坏"))?;
            if manifest.schema_version != 1 {
                return Err(invalid("未知存储版本"));
            }
            for path in manifest.paths {
                path_kind(&path)?;
                let mut part = dir.clone();
                for component in path.split('/') {
                    part.push(component);
                    plain(&part)?;
                }
                if state
                    .documents
                    .insert(path, fs::read_to_string(part)?)
                    .is_some()
                {
                    return Err(invalid("重复存储路径"));
                }
            }
            state.receipts = manifest.receipts;
            validate_all(&state.documents)?;
            revision = Some(id.trim().to_owned());
        }
        Ok(Self {
            last_commit_ms: None,
            root,
            state,
            revision,
            _lock: f,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// In-memory counts; callers can release the store mutex before scanning disk.
    pub fn usage_snapshot(&self) -> Value {
        serde_json::json!({"documents":self.state.documents.len(),"document_bytes":self.state.documents.values().map(String::len).sum::<usize>(),"receipts":self.state.receipts.len(),"revision":self.revision,"document_limit":4096,"document_bytes_limit":32*1024*1024,"maintenance":"只读统计；未执行GC或删除幂等回执"})
    }
    /// Explicit read-only maintenance report; never runs on the write path.
    pub fn usage(&self) -> Result<Value> {
        let mut result = self.usage_snapshot();
        result
            .as_object_mut()
            .unwrap()
            .extend(Self::disk_usage(&self.root)?.as_object().unwrap().clone());
        Ok(result)
    }
    /// Approximate live disk usage; does not require the business store mutex.
    pub fn disk_usage(root: &Path) -> Result<Value> {
        fn scan(path: &Path) -> std::io::Result<u64> {
            let mut size = 0;
            for entry in fs::read_dir(path)? {
                let e = entry?;
                let kind = e.file_type()?;
                if kind.is_symlink() {
                    continue;
                }
                size += if kind.is_dir() {
                    scan(&e.path())?
                } else {
                    e.metadata()?.len()
                };
            }
            Ok(size)
        }
        let revisions = fs::read_dir(root.join("revisions"))?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .count();
        Ok(
            serde_json::json!({"revisions":revisions,"disk_bytes":scan(&root.join("revisions"))?,"disk_scope":"全局实时近似值，扫描不占业务锁，可能包含正在写入的修订"}),
        )
    }

    pub fn transact(
        &mut self,
        key: &str,
        request: &Value,
        operation: impl FnOnce(&mut State) -> Result<Value>,
    ) -> Result<Value> {
        self.last_commit_ms = None;
        if key.is_empty()
            || key.len() > 128
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(invalid("写请求需要有效Idempotency-Key（1–128字）"));
        }
        let hash = sha(&request.to_string());
        if let Some(receipt) = self.state.receipts.get(key) {
            if receipt.hash != hash {
                return Err(conflict("同一幂等键已用于不同请求"));
            }
            let mut result = receipt.response.clone();
            result["idempotent"] = Value::Bool(true);
            return Ok(result);
        }
        let mut next = self.state.clone();
        let mut response = operation(&mut next)?;
        if response.get("mode").and_then(Value::as_str) == Some("SKIP") {
            return Ok(response);
        }
        validate_all(&next.documents)?;
        response["idempotent"] = Value::Bool(false);
        next.receipts.insert(
            key.to_owned(),
            Receipt {
                hash,
                response: response.clone(),
            },
        );
        let commit_started = std::time::Instant::now();
        let committed = self.commit(next);
        self.last_commit_ms = Some(commit_started.elapsed().as_secs_f64() * 1000.0);
        committed?;
        Ok(response)
    }
    pub fn import(&mut self, documents: Documents) -> Result<()> {
        if self.revision.is_some() || !self.state.documents.is_empty() {
            return Err(conflict("只允许导入空存储；不能覆盖服务端已有数据"));
        }
        validate_all(&documents)?;
        self.commit(State {
            documents,
            receipts: BTreeMap::new(),
        })
    }
    fn commit(&mut self, next: State) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let dir = self.root.join("revisions").join(&id);
        fs::create_dir(&dir)?;
        for (relative, content) in &next.documents {
            path_kind(relative)?;
            let target = dir.join(relative);
            fs::create_dir_all(target.parent().unwrap())?;
            exclusive_write(&target, content.as_bytes())?;
        }
        let manifest = Manifest {
            schema_version: 1,
            paths: next.documents.keys().cloned().collect(),
            receipts: next.receipts.clone(),
        };
        exclusive_write(
            &dir.join(".meta.json"),
            &serde_json::to_vec(&manifest).map_err(|_| invalid("存储清单无法序列化"))?,
        )?;
        #[cfg(unix)]
        {
            sync_dirs(&dir)?;
            // Persist the revision's directory entry before HEAD can reference it.
            File::open(self.root.join("revisions"))?.sync_all()?;
        }
        let tmp = self.root.join(format!(".head-{id}"));
        exclusive_write(&tmp, format!("{id}\n").as_bytes())?;
        fs::rename(tmp, self.root.join("HEAD"))?;
        // After publishing HEAD, retries must see the durable receipt, even if directory fsync reports an error.
        self.state = next;
        self.revision = Some(id);
        #[cfg(unix)]
        {
            File::open(&self.root)?.sync_all()?;
        }
        Ok(())
    }
}
#[cfg(unix)]
fn sync_dirs(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            sync_dirs(&path)?;
        }
    }
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub fn import_directory(source: &Path) -> Result<Documents> {
    fn walk(root: &Path, dir: &Path, docs: &mut Documents) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(invalid("导入目录不能包含符号链接"));
            }
            if file_type.is_dir() {
                walk(root, &entry.path(), docs)?;
            } else if file_type.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if path_kind(&relative).is_ok() {
                    docs.insert(relative, fs::read_to_string(path)?);
                }
            }
        }
        Ok(())
    }
    let mut docs = Documents::new();
    for relative in [
        "namespaces",
        "control",
        "namespaces",
        "memory/project",
        "memory/process",
        "memory/templates",
        "memory/logs",
        "handoffs/sessions",
        "handoffs/workstreams",
    ] {
        let path = source.join(relative);
        if path.exists() {
            plain(&path)?;
            walk(source, &path, &mut docs)?;
        }
    }
    if docs.is_empty() {
        return Err(invalid("来源没有受管Markdown文档"));
    }
    validate_all(&docs)?;
    Ok(docs)
}
