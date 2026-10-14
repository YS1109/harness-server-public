use crate::{
    document::{Documents, path_kind, validate_all},
    error::{Result, conflict, invalid},
    store::Receipt,
};
use chrono::Utc;
use flate2::{Compression, write::GzEncoder};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub data_dir: PathBuf,
    pub output_dir: PathBuf,
    pub oss: Option<OssConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OssConfig {
    pub enabled: bool,
    pub bucket: String,
    pub endpoint: String,
    pub prefix: String,
    pub ossutil: PathBuf,
    pub credentials_file: Option<PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        regular(path)?;
        let config: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|_| invalid("备份配置无效或包含不支持的字段；不在JSON中保存凭据"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || !self.data_dir.is_absolute()
            || !self.output_dir.is_absolute()
        {
            return Err(invalid("备份配置需要schema_version=1及绝对数据/输出目录"));
        }
        if [&self.data_dir, &self.output_dir]
            .iter()
            .any(|path| path.components().any(|c| matches!(c, Component::ParentDir)))
        {
            return Err(invalid("备份目录不能包含上级路径跳转"));
        }
        if let Some(oss) = &self.oss
            && oss.enabled
        {
            oss.validate()?;
        }
        Ok(())
    }
}

impl OssConfig {
    fn validate(&self) -> Result<()> {
        let endpoint =
            url::Url::parse(&self.endpoint).map_err(|_| invalid("OSS endpoint必须是HTTPS地址"))?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.path(), "" | "/")
        {
            return Err(invalid("OSS endpoint必须是无凭据、无路径参数的HTTPS地址"));
        }
        if !(3..=63).contains(&self.bucket.len())
            || !self
                .bucket
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || self.bucket.starts_with('-')
            || self.bucket.ends_with('-')
        {
            return Err(invalid("OSS bucket名称无效"));
        }
        if self.prefix.is_empty()
            || self.prefix.len() > 512
            || self
                .prefix
                .split('/')
                .any(|p| p.is_empty() || matches!(p, "." | ".."))
            || !self
                .prefix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c))
            || self.ossutil.as_os_str().is_empty()
            || (!self.ossutil.is_absolute() && self.ossutil.components().count() != 1)
        {
            return Err(invalid(
                "OSS前缀须为非空相对对象路径，ossutil须为可执行文件路径或命令名",
            ));
        }
        if let Some(path) = &self.credentials_file
            && !path.is_absolute()
        {
            return Err(invalid("OSS凭据配置文件必须使用绝对路径"));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RevisionManifest {
    schema_version: u32,
    paths: Vec<String>,
    receipts: BTreeMap<String, Receipt>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub schema_version: u32,
    pub created_at: String,
    pub revision: String,
    pub documents: usize,
    pub receipts: usize,
    pub revisions: usize,
    pub excluded_incomplete_revisions: Vec<String>,
    pub archive: String,
    pub sha256: String,
    pub bytes: u64,
    pub scope: String,
}

fn regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid("备份不接受符号链接或非普通文件"));
    }
    Ok(())
}

fn directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(invalid("备份目录不接受符号链接"));
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn output(config: &Config) -> Result<(PathBuf, File)> {
    config.validate()?;
    // Verify/upload must remain possible when the source store has been lost.
    let source = config
        .data_dir
        .canonicalize()
        .unwrap_or_else(|_| config.data_dir.clone());
    // Resolve the existing parent before creation, so data-dir nesting is rejected without mutation.
    let mut ancestor = config.output_dir.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| invalid("备份输出目录无效"))?;
    }
    let resolved = ancestor
        .canonicalize()?
        .join(config.output_dir.strip_prefix(ancestor).unwrap());
    if resolved.starts_with(&source) || source.starts_with(&resolved) {
        return Err(invalid("备份输出目录与数据目录不能相同或互相包含"));
    }
    fs::create_dir_all(&config.output_dir)?;
    directory(&config.output_dir)?;
    let dest = config.output_dir.canonicalize()?;
    if dest.starts_with(&source) || source.starts_with(&dest) {
        return Err(invalid("备份输出目录与数据目录不能相同或互相包含"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o700))?;
    }
    let lock_path = dest.join(".backup.lock");
    if lock_path.exists() {
        regular(&lock_path)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|_| conflict("另一个备份/上传正在使用该输出目录"))?;
    Ok((dest, lock))
}

struct Snapshot {
    revision: String,
    documents: usize,
    receipts: usize,
    revisions: usize,
    excluded: Vec<String>,
    files: Vec<(PathBuf, String)>,
}

fn capture(source: &Path) -> Result<Snapshot> {
    directory(source)?;
    regular(&source.join("HEAD"))?;
    let revision = fs::read_to_string(source.join("HEAD"))?.trim().to_owned();
    Uuid::parse_str(&revision).map_err(|_| invalid("存储HEAD损坏"))?;
    let revisions = source.join("revisions");
    directory(&revisions)?;
    let mut ids = BTreeSet::new();
    for entry in fs::read_dir(&revisions)? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().to_string();
        Uuid::parse_str(&id).map_err(|_| invalid("修订目录含未知条目；未备份"))?;
        directory(&entry.path())?;
        ids.insert(id);
    }
    ids.insert(revision.clone());
    let mut snapshot = Snapshot {
        revision: revision.clone(),
        documents: 0,
        receipts: 0,
        revisions: 0,
        excluded: Vec::new(),
        files: Vec::new(),
    };
    for id in ids {
        let dir = revisions.join(&id);
        directory(&dir)?;
        let manifest_path = dir.join(".meta.json");
        if !manifest_path.exists() && id != revision {
            snapshot.excluded.push(id);
            continue;
        }
        regular(&manifest_path)?;
        let manifest: RevisionManifest = serde_json::from_slice(&fs::read(&manifest_path)?)
            .map_err(|_| invalid("修订清单损坏或正在提交；可稍后重试，未生成有效备份"))?;
        if manifest.schema_version != 1 || manifest.paths.len() > 4096 {
            return Err(invalid("修订清单版本或容量无效"));
        }
        let mut seen = BTreeSet::new();
        let mut current = Documents::new();
        for path in manifest.paths {
            path_kind(&path)?;
            if !seen.insert(path.clone()) {
                return Err(invalid("修订清单包含重复文档"));
            }
            let mut target = dir.clone();
            let parts: Vec<_> = path.split('/').collect();
            for (i, part) in parts.iter().enumerate() {
                target.push(part);
                if i + 1 == parts.len() {
                    regular(&target)?;
                } else {
                    directory(&target)?;
                }
            }
            if id == revision {
                current.insert(path.clone(), fs::read_to_string(&target)?);
            }
            snapshot
                .files
                .push((target, format!("revisions/{id}/{path}")));
        }
        if id == revision {
            validate_all(&current)?;
            snapshot.documents = current.len();
            snapshot.receipts = manifest.receipts.len();
        }
        snapshot
            .files
            .push((manifest_path, format!("revisions/{id}/.meta.json")));
        snapshot.revisions += 1;
    }
    Ok(snapshot)
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    regular(path)?;
    let mut f = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut bytes = 0;
    loop {
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        bytes += n as u64;
    }
    Ok((format!("{:x}", hash.finalize()), bytes))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension(format!("{}-part", Uuid::new_v4()));
    let result = (|| {
        let mut f = private_file(&tmp)?;
        f.write_all(
            &serde_json::to_vec_pretty(value).map_err(|_| invalid("备份元数据无法序列化"))?,
        )?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn archive_snapshot(snapshot: &Snapshot, partial: &Path) -> Result<()> {
    let encoder = GzEncoder::new(private_file(partial)?, Compression::default());
    let mut tar = tar::Builder::new(encoder);
    let head = format!("{}\n", snapshot.revision);
    let mut header = tar::Header::new_gnu();
    header.set_size(head.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    tar.append_data(&mut header, "HEAD", head.as_bytes())?;
    for (source, relative) in &snapshot.files {
        regular(source)?;
        let mut file = File::open(source)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(file.metadata()?.len());
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, relative, &mut file)?;
    }
    tar.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

pub fn create(config: &Config, local_only: bool) -> Result<Value> {
    let (dest, _lock) = output(config)?;
    let snapshot = capture(&config.data_dir)?;
    let name = format!(
        "harness-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        Uuid::new_v4()
    );
    let archive = dest.join(format!("{name}.tar.gz"));
    let partial = dest.join(format!(".{name}.part"));
    let result = (|| -> Result<()> {
        archive_snapshot(&snapshot, &partial)?;
        fs::rename(&partial, &archive)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result?;
    let (sha256, bytes) = hash_file(&archive)?;
    let manifest = BackupManifest {
        schema_version: 1, created_at: Utc::now().to_rfc3339(), revision: snapshot.revision,
        documents: snapshot.documents, receipts: snapshot.receipts, revisions: snapshot.revisions,
        excluded_incomplete_revisions: snapshot.excluded, archive: format!("{name}.tar.gz"), sha256, bytes,
        scope: "固定HEAD的全部文档与幂等回执，以及扫描到的完整修订；不含凭据、进程统计或未完成修订。要求修订不可变且无并发GC/恢复/迁移。".into(),
    };
    let manifest_path = dest.join(format!("{name}.json"));
    write_json(&manifest_path, &manifest)?;
    let mut result = json!({"status":"local_backup_created","manifest":manifest_path,"backup":manifest,"uploaded":false});
    if !local_only && config.oss.as_ref().is_some_and(|o| o.enabled) {
        result["upload"] = upload_verified(config, &manifest_path, &manifest).map_err(|error| {
            invalid(format!(
                "本地备份已完成，清单 {}；{error}",
                manifest_path.display()
            ))
        })?;
        result["uploaded"] = json!(true);
        result["status"] = json!("backup_uploaded");
    }
    Ok(result)
}

fn verified_manifest(dest: &Path, path: &Path) -> Result<BackupManifest> {
    regular(path)?;
    let path = path.canonicalize()?;
    if path.parent() != Some(dest) {
        return Err(invalid("备份清单必须位于配置的输出目录"));
    }
    let manifest: BackupManifest =
        serde_json::from_slice(&fs::read(&path)?).map_err(|_| invalid("备份清单无效"))?;
    let name = Path::new(&manifest.archive);
    if manifest.schema_version != 1
        || name.components().count() != 1
        || name.file_name().is_none()
        || !manifest.archive.starts_with("harness-")
        || !manifest.archive.ends_with(".tar.gz")
    {
        return Err(invalid("备份清单版本或归档文件名无效"));
    }
    Uuid::parse_str(&manifest.revision).map_err(|_| invalid("备份修订ID无效"))?;
    chrono::DateTime::parse_from_rfc3339(&manifest.created_at)
        .map_err(|_| invalid("备份时间无效"))?;
    let (hash, bytes) = hash_file(&dest.join(&manifest.archive))?;
    if hash != manifest.sha256 || bytes != manifest.bytes {
        return Err(invalid("备份大小或SHA-256不一致；未上传"));
    }
    Ok(manifest)
}

pub fn verify(config: &Config, path: &Path) -> Result<Value> {
    let (dest, _lock) = output(config)?;
    let manifest = verified_manifest(&dest, path)?;
    Ok(
        json!({"status":"checksum_verified","backup":manifest,"scope":"仅核验本地归档大小与SHA-256；恢复演练另行执行"}),
    )
}

pub fn upload(config: &Config, path: &Path) -> Result<Value> {
    let (dest, _lock) = output(config)?;
    let manifest = verified_manifest(&dest, path)?;
    upload_verified(config, &path.canonicalize()?, &manifest)
}

fn upload_verified(config: &Config, path: &Path, manifest: &BackupManifest) -> Result<Value> {
    let oss = config
        .oss
        .as_ref()
        .filter(|o| o.enabled)
        .ok_or_else(|| invalid("OSS上传未启用；本地备份可保留并稍后重试"))?;
    oss.validate()?;
    if let Some(path) = &oss.credentials_file {
        regular(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
                return Err(invalid("OSS凭据配置文件不能对组或其他用户开放"));
            }
        }
    }
    let folder = manifest
        .created_at
        .get(..10)
        .ok_or_else(|| invalid("备份时间无效"))?;
    let mut objects = Vec::new();
    for source in [
        path.parent().unwrap().join(&manifest.archive),
        path.to_path_buf(),
    ] {
        let filename = source
            .file_name()
            .unwrap()
            .to_str()
            .ok_or_else(|| invalid("备份文件名无效"))?;
        let object = format!(
            "oss://{}/{}/{}/{}",
            oss.bucket, oss.prefix, folder, filename
        );
        let mut cmd = Command::new(&oss.ossutil);
        cmd.current_dir(path.parent().unwrap());
        cmd.arg("--endpoint").arg(&oss.endpoint);
        if let Some(credentials) = &oss.credentials_file {
            cmd.arg("--config-file").arg(credentials);
        }
        let result = cmd
            .arg("cp")
            .arg(&source)
            .arg(&object)
            .arg("--force")
            .output()
            .map_err(|_| invalid("无法启动ossutil；本地备份保留，可配置后upload重试"))?;
        if !result.status.success() {
            return Err(invalid(format!(
                "OSS上传失败（退出码 {:?}）；本地备份保留，用原清单重试；不输出可能含凭据的ossutil日志",
                result.status.code()
            )));
        }
        objects.push(object);
    }
    let result = json!({"status":"uploaded","objects":objects,"verification":"ossutil两次上传成功退出；远端下载后的SHA与恢复演练尚未验证"});
    write_json(&path.with_extension("uploaded.json"), &result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::render, store::Store};

    #[test]
    fn committing_after_capture_does_not_change_the_archived_head_or_documents() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("data");
        let mut store = Store::open(&source).unwrap();
        let content = render(json!({"schema_version":1,"memory_type":"project","project_id":"demo","title":"Demo","status":"ACTIVE","verified_at":"unknown"}).as_object().unwrap(), "# Before\n").unwrap();
        let mut docs = Documents::new();
        docs.insert("memory/project/demo.md".into(), content.clone());
        store.import(docs).unwrap();
        let snapshot = capture(&source).unwrap();
        store
            .transact("new-write", &json!({"new":true}), |s| {
                s.documents.insert(
                    "memory/project/demo.md".into(),
                    content.replace("Before", "After"),
                );
                Ok(json!({"written":true}))
            })
            .unwrap();
        assert_ne!(store.revision.as_deref(), Some(snapshot.revision.as_str()));
        let path = temp.path().join("snapshot.tar.gz");
        archive_snapshot(&snapshot, &path).unwrap();
        let restored_dir = temp.path().join("restored");
        fs::create_dir(&restored_dir).unwrap();
        tar::Archive::new(flate2::read::GzDecoder::new(File::open(&path).unwrap()))
            .unpack(&restored_dir)
            .unwrap();
        let restored = Store::open(&restored_dir).unwrap();
        assert_eq!(
            restored.revision.as_deref(),
            Some(snapshot.revision.as_str())
        );
        assert_eq!(restored.state.documents["memory/project/demo.md"], content);
        assert!(
            !restored_dir
                .join("revisions")
                .join(store.revision.as_ref().unwrap())
                .exists()
        );
    }
}
