//! Best-effort local telemetry. Never part of the knowledge transaction or backups.
use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

const MAX_DAY_BYTES: u64 = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub service_version: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub execution_group_id: Option<String>,
    #[serde(default)]
    pub lock_wait_ms: Option<f64>,
    #[serde(default)]
    pub operation_ms: Option<f64>,
    #[serde(default)]
    pub commit_ms: Option<f64>,
    pub timestamp: DateTime<Utc>,
    pub namespace: String,
    pub command: String,
    pub project: Option<String>,
    pub handler_ms: f64,
    pub success: bool,
    pub retry: bool,
    pub error_code: Option<String>,
    pub returned_characters: Option<u64>,
    pub handoff_characters: Option<u64>,
}
#[derive(Default, Serialize)]
pub struct Summary {
    pub observed_calls: u64,
    pub failures: u64,
    pub retries: u64,
    pub handler_ms_total: f64,
    pub handler_ms_p50: Option<f64>,
    pub handler_ms_p90: Option<f64>,
    pub returned_characters: Option<u64>,
    pub returned_character_observed_calls: u64,
    pub handoff_characters: Option<u64>,
    pub handoff_character_observed_calls: u64,
    pub commands: BTreeMap<String, u64>,
    pub error_codes: BTreeMap<String, u64>,
    pub service_versions: BTreeMap<String, u64>,
    pub protocols: BTreeMap<String, u64>,
    pub grouped_calls: u64,
    pub execution_groups: usize,
    pub lock_wait_ms_p50: Option<f64>,
    pub lock_wait_ms_p90: Option<f64>,
    pub operation_ms_p50: Option<f64>,
    pub operation_ms_p90: Option<f64>,
    pub commit_ms_p50: Option<f64>,
    pub commit_ms_p90: Option<f64>,
    pub commit_observed_calls: u64,
    pub lock_wait_observed_calls: u64,
    pub operation_observed_calls: u64,
    #[serde(skip)]
    groups: std::collections::BTreeSet<String>,
    #[serde(skip)]
    lock_timings: Vec<f64>,
    #[serde(skip)]
    operation_timings: Vec<f64>,
    #[serde(skip)]
    commit_timings: Vec<f64>,
    #[serde(skip)]
    timings: Vec<f64>,
}
impl Summary {
    fn add(&mut self, e: &Event) {
        if let Some(group) = &e.execution_group_id {
            self.grouped_calls += 1;
            self.groups.insert(group.clone());
        }
        *self
            .protocols
            .entry(e.protocol.clone().unwrap_or_else(|| "unknown".into()))
            .or_default() += 1;
        for (value, rows) in [
            (e.lock_wait_ms, &mut self.lock_timings),
            (e.operation_ms, &mut self.operation_timings),
            (e.commit_ms, &mut self.commit_timings),
        ] {
            if let Some(ms) = value.filter(|n| n.is_finite() && *n >= 0.0) {
                rows.push(ms);
            }
        }
        self.observed_calls += 1;
        self.failures += u64::from(!e.success);
        self.retries += u64::from(e.retry);
        self.handler_ms_total += e.handler_ms;
        self.timings.push(e.handler_ms);
        *self.commands.entry(e.command.clone()).or_default() += 1;
        *self
            .service_versions
            .entry(
                e.service_version
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
            )
            .or_default() += 1;
        if let Some(code) = &e.error_code {
            *self.error_codes.entry(code.clone()).or_default() += 1;
        }
        if let Some(n) = e.returned_characters {
            *self.returned_characters.get_or_insert(0) += n;
            self.returned_character_observed_calls += 1;
        }
        if let Some(n) = e.handoff_characters {
            *self.handoff_characters.get_or_insert(0) += n;
            self.handoff_character_observed_calls += 1;
        }
    }
    fn finish(&mut self) {
        fn quantiles(rows: &mut [f64]) -> (Option<f64>, Option<f64>) {
            rows.sort_by(f64::total_cmp);
            let n = rows.len();
            if n == 0 {
                (None, None)
            } else {
                (
                    Some((rows[(n - 1) / 2] + rows[n / 2]) / 2.0),
                    Some(rows[(9 * n).div_ceil(10) - 1]),
                )
            }
        }
        (self.lock_wait_ms_p50, self.lock_wait_ms_p90) = quantiles(&mut self.lock_timings);
        (self.operation_ms_p50, self.operation_ms_p90) = quantiles(&mut self.operation_timings);
        (self.commit_ms_p50, self.commit_ms_p90) = quantiles(&mut self.commit_timings);
        self.commit_observed_calls = self.commit_timings.len() as u64;
        self.lock_wait_observed_calls = self.lock_timings.len() as u64;
        self.operation_observed_calls = self.operation_timings.len() as u64;
        self.execution_groups = self.groups.len();
        self.timings.sort_by(f64::total_cmp);
        let n = self.timings.len();
        if n > 0 {
            self.handler_ms_p50 = Some((self.timings[(n - 1) / 2] + self.timings[n / 2]) / 2.0);
            self.handler_ms_p90 = Some(self.timings[(9 * n).div_ceil(10) - 1]);
        }
    }
}
pub struct Ledger {
    directory: PathBuf,
    pub started: Instant,
    dropped: AtomicU64,
    generation: AtomicU64,
    cache: Mutex<BTreeMap<String, (Instant, u64, Value)>>,
}
impl Ledger {
    pub fn new(root: &Path) -> Self {
        Self {
            directory: root.join(".runtime/metrics"),
            started: Instant::now(),
            dropped: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            cache: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn record(&self, event: &Event) {
        if self.append(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        // A report racing with append cannot cache an older snapshot as current.
        self.generation.fetch_add(1, Ordering::Release);
    }
    fn append(&self, e: &Event) -> std::io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let path = self
            .directory
            .join(format!("{}.jsonl", e.timestamp.format("%Y-%m-%d")));
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true).read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.try_lock_exclusive()?;
        if file.metadata()?.len() >= MAX_DAY_BYTES {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        let mut line = serde_json::to_vec(e)?;
        line.push(b'\n');
        file.write_all(&line)
    }
    pub fn report(
        &self,
        days: u32,
        namespace: Option<&str>,
        project: Option<&str>,
        now: DateTime<Utc>,
    ) -> Value {
        let cache_key =
            serde_json::to_string(&(days, namespace, project)).expect("serializable cache key");
        let generation = self.generation.load(Ordering::Acquire);
        if let Ok(cache) = self.cache.lock() {
            if let Some((time, _, r)) = cache.get(&cache_key).filter(|(t, g, r)| {
                let sample = r["sample_at"]
                    .as_str()
                    .and_then(|s| s.parse::<DateTime<Utc>>().ok());
                *g == generation
                    && t.elapsed().as_secs() < 30
                    && sample.is_some_and(|s| now >= s && now - s < Duration::seconds(30))
            }) {
                let mut result = r.clone();
                result["cache_hit"] = json!(true);
                result["cache_age_seconds"] = json!(time.elapsed().as_secs());
                return result;
            }
        }
        let cutoff = now - Duration::days(i64::from(days));
        let mut all = Summary::default();
        let mut groups = BTreeMap::<String, Summary>::new();
        let mut commands = BTreeMap::<String, Summary>::new();
        let mut unreadable = 0u64;
        let mut malformed = 0u64;
        let mut first: Option<DateTime<Utc>> = None;
        // Enumerate only the requested dates; no historical directory scan on business calls.
        for day in 0..=days {
            let date = (now - Duration::days(i64::from(day)))
                .format("%Y-%m-%d")
                .to_string();
            let path = self.directory.join(format!("{date}.jsonl"));
            let file = match fs::File::open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    unreadable += 1;
                    continue;
                }
            };
            if file
                .metadata()
                .map_or(true, |m| m.len() > MAX_DAY_BYTES + 4096)
            {
                unreadable += 1;
                continue;
            }
            for line in BufReader::new(file).lines() {
                let event: Event = match line.ok().and_then(|s| serde_json::from_str(&s).ok()) {
                    Some(e) => e,
                    None => {
                        malformed += 1;
                        continue;
                    }
                };
                if event.timestamp < cutoff
                    || event.timestamp > now
                    || namespace.is_some_and(|n| n != event.namespace)
                    || project.is_some_and(|p| event.project.as_deref() != Some(p))
                {
                    continue;
                }
                if !event.handler_ms.is_finite() || event.handler_ms < 0.0 {
                    malformed += 1;
                    continue;
                }
                first = Some(first.map_or(event.timestamp, |old| old.min(event.timestamp)));
                groups
                    .entry(event.namespace.clone())
                    .or_default()
                    .add(&event);
                commands
                    .entry(event.command.clone())
                    .or_default()
                    .add(&event);
                all.add(&event);
            }
        }
        all.finish();
        for s in commands.values_mut() {
            s.finish();
        }
        let groups: Vec<_> = groups
            .into_iter()
            .map(|(id, mut s)| {
                s.finish();
                json!({"id":id,"metrics":s})
            })
            .collect();
        let result = json!({"sample_at":now,"cache_hit":false,"cache_age_seconds":0,"command_metrics":commands,"metrics":all,"namespaces":groups,"window_days":days,"observed_since":first,"window_start":cutoff,"coverage_scope":"全局采集健康；无法归属空间的损坏记录与进程丢弃不按空间拆分。","coverage":{"malformed_events":malformed,"unreadable_files":unreadable,"dropped_events_this_process":self.dropped.load(Ordering::Relaxed)},"metrics_scope":"仅成功采集且进入业务处理器的调用；服务业务处理耗时含锁等待、逻辑与提交，不含鉴权/解析、执行队列等待、采集追加、模型思考、客户端启动及网络往返；鉴权/解析失败和health/stats/daily不采集。历史不会从旧日志补算。","net_benefit":"尚未测量"});
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= 32 {
                cache.clear();
            }
            cache.insert(cache_key, (Instant::now(), generation, result.clone()));
        }
        result
    }
}

impl Ledger {
    /// Explicit offline maintenance. The server command holds the Store writer lock.
    pub fn retention(&self, apply: bool, now: DateTime<Utc>) -> std::io::Result<Value> {
        let cutoff = (now - Duration::days(90)).date_naive();
        let mut expired = Vec::new();
        let entries = match fs::read_dir(&self.directory) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({"expired_files":0,"deleted_files":0,"apply":apply}));
            }
            Err(e) => return Err(e),
        };
        for entry in entries {
            let e = entry?;
            if !e.file_type()?.is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                if chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d").is_ok_and(|d| d < cutoff) {
                    expired.push(e.path());
                }
            }
        }
        let mut deleted = 0;
        if apply {
            for file in &expired {
                fs::remove_file(file)?;
                deleted += 1;
            }
        }
        Ok(
            json!({"expired_files":expired.len(),"deleted_files":deleted,"apply":apply,"retention_days":90}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(now: DateTime<Utc>, ms: f64, ns: &str) -> Event {
        Event {
            event_id: Some(uuid::Uuid::new_v4().to_string()),
            service_version: Some(env!("CARGO_PKG_VERSION").into()),
            protocol: Some("v1".into()),
            execution_group_id: None,
            lock_wait_ms: None,
            operation_ms: None,
            commit_ms: None,
            timestamp: now,
            namespace: ns.into(),
            command: "route".into(),
            project: Some("demo".into()),
            handler_ms: ms,
            success: true,
            retry: false,
            error_code: None,
            returned_characters: None,
            handoff_characters: None,
        }
    }
    #[test]
    fn windows_percentiles_unknowns_and_corruption_are_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(temp.path());
        let now = Utc::now();
        for n in [10., 20., 30., 40.] {
            ledger.record(&event(now, n, "private"));
        }
        ledger.record(&event(now - Duration::days(8), 999., "private"));
        ledger.record(&event(now, 888., "other"));
        let path = ledger
            .directory
            .join(format!("{}.jsonl", now.format("%Y-%m-%d")));
        writeln!(
            OpenOptions::new().append(true).open(path).unwrap(),
            "broken"
        )
        .unwrap();
        let r = ledger.report(7, Some("private"), None, now);
        assert_eq!(r["metrics"]["observed_calls"], 4);
        assert_eq!(r["metrics"]["handler_ms_p50"], 25.0);
        assert_eq!(r["metrics"]["handler_ms_p90"], 40.0);
        assert_eq!(r["coverage"]["malformed_events"], 1);
        assert!(r["metrics"]["returned_characters"].is_null());
        assert_eq!(
            ledger.report(30, Some("private"), None, now)["metrics"]["observed_calls"],
            5
        );
        assert_eq!(
            ledger.report(7, Some("missing"), None, now)["metrics"]["observed_calls"],
            0
        );
    }
    #[test]
    fn telemetry_failures_do_not_panic_and_lock_contention_is_not_waited_on() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(temp.path());
        let now = Utc::now();
        ledger.record(&event(now, 1., "shared"));
        let file = OpenOptions::new()
            .append(true)
            .open(
                ledger
                    .directory
                    .join(format!("{}.jsonl", now.format("%Y-%m-%d"))),
            )
            .unwrap();
        file.lock_exclusive().unwrap();
        let t = Instant::now();
        ledger.record(&event(now, 2., "shared"));
        assert!(t.elapsed().as_secs_f64() < 1.0);
        assert_eq!(ledger.dropped.load(Ordering::Relaxed), 1);
        drop(file);
        let broken = temp.path().join("broken");
        fs::create_dir_all(broken.join(".runtime")).unwrap();
        fs::write(broken.join(".runtime/metrics"), "not a directory").unwrap();
        let ledger = Ledger::new(&broken);
        ledger.record(&event(now, 3., "shared"));
        assert_eq!(ledger.dropped.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn legacy_stage_samples_remain_unknown_and_cache_respects_time_and_append() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(temp.path());
        let now = Utc::now();
        let mut legacy = serde_json::to_value(event(now, 10.0, "shared")).unwrap();
        for field in [
            "protocol",
            "execution_group_id",
            "lock_wait_ms",
            "operation_ms",
            "commit_ms",
        ] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        ledger.record(&serde_json::from_value(legacy).unwrap());
        let first = ledger.report(7, None, None, now);
        assert_eq!(first["metrics"]["protocols"]["unknown"], 1);
        assert!(first["metrics"]["commit_ms_p50"].is_null());
        assert_eq!(first["metrics"]["commit_observed_calls"], 0);
        assert_eq!(ledger.report(7, None, None, now)["cache_hit"], true);
        let later = ledger.report(7, None, None, now + Duration::days(8));
        assert_eq!(later["cache_hit"], false);
        assert_eq!(later["metrics"]["observed_calls"], 0);
        assert_eq!(ledger.report(7, None, None, now)["cache_hit"], false);
        let mut measured = event(now, 20.0, "shared");
        measured.lock_wait_ms = Some(2.0);
        measured.operation_ms = Some(5.0);
        measured.commit_ms = Some(13.0);
        ledger.record(&measured);
        let updated = ledger.report(7, None, None, now);
        assert_eq!(updated["cache_hit"], false);
        assert_eq!(updated["metrics"]["observed_calls"], 2);
        assert_eq!(updated["metrics"]["lock_wait_observed_calls"], 1);
        assert_eq!(updated["metrics"]["operation_observed_calls"], 1);
        assert_eq!(updated["metrics"]["commit_ms_p50"], 13.0);
    }
    #[test]
    fn retention_is_explicit_and_preserves_current_and_unrelated_files() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(temp.path());
        let now = Utc::now();
        ledger.record(&event(now, 1., "shared"));
        let old = ledger.directory.join(format!(
            "{}.jsonl",
            (now - Duration::days(100)).format("%Y-%m-%d")
        ));
        fs::write(&old, "old").unwrap();
        fs::write(ledger.directory.join("notes.txt"), "keep").unwrap();
        assert_eq!(ledger.retention(false, now).unwrap()["expired_files"], 1);
        assert!(old.exists());
        assert_eq!(ledger.retention(true, now).unwrap()["deleted_files"], 1);
        assert!(!old.exists());
        assert!(ledger.directory.join("notes.txt").exists());
    }
    #[test]
    #[ignore = "manual local append-only cost baseline; not a task performance test"]
    fn append_cost_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(temp.path());
        let sample = event(Utc::now(), 1., "shared");
        let n = 1000;
        let start = Instant::now();
        for _ in 0..n {
            std::hint::black_box(&sample);
        }
        let off = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        for _ in 0..n {
            ledger.record(&sample);
        }
        let on = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(ledger.dropped.load(Ordering::Relaxed), 0);
        println!(
            "{{\"samples\":{n},\"collection_off_ms\":{off},\"collection_on_ms\":{on},\"append_ms_per_call\":{},\"scope\":\"local cached filesystem append, debug build; excludes business handler, network and process startup\"}}",
            (on - off) / f64::from(n)
        );
    }
}
