use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;

const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;

type Signature = Vec<(String, u128, u64)>;

#[derive(Debug)]
pub struct AppendOutcome {
    pub sample_id: SampleId,
    pub retention: Result<RetentionOutcome, FineTuneError>,
}

struct DropRewrite {
    removed_ids: Vec<SampleId>,
    unreadable: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct RatingUpdate {
    pub previous: Option<Rating>,
    pub current: Rating,
}

#[derive(Debug, Clone, Copy)]
pub struct ClearConfirmation {
    _private: (),
}

impl ClearConfirmation {
    pub fn confirmed() -> Self {
        Self { _private: () }
    }
}

#[derive(Clone)]
pub struct FineTuneStore {
    cancellation: tokio_util::sync::CancellationToken,
    root: Arc<PathBuf>,
    io_lock: Arc<Mutex<()>>,
    stats_cache: Arc<Mutex<Option<(Signature, Stats)>>>,
}

impl FineTuneStore {
    pub fn open(root: PathBuf) -> Result<Self, FineTuneError> {
        if !root.is_absolute() {
            return Err(FineTuneError::RelativeDirectory(root));
        }
        if root.exists() && !root.is_dir() {
            return Err(FineTuneError::NotDirectory(root));
        }
        fs::create_dir_all(&root).map_err(|error| io_error(&root, error))?;
        Ok(Self {
            cancellation: tokio_util::sync::CancellationToken::new(),
            root: Arc::new(root),
            io_lock: Arc::new(Mutex::new(())),
            stats_cache: Arc::new(Mutex::new(None)),
        })
    }

    pub(super) fn with_cancellation(
        mut self,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Appends one provider-response sample and then applies retention. A
    /// retention failure is returned separately because the sample was already
    /// persisted successfully.
    pub fn append_sample(
        &self,
        sample: &FineTuneSample,
        retention: Retention,
    ) -> Result<AppendOutcome, FineTuneError> {
        let _guard = self.guard()?;
        self.append_json_locked("samples", sample.timestamp.month_key(), sample)?;
        let retention = self.enforce_retention_locked(retention);
        self.invalidate_cache();
        Ok(AppendOutcome {
            sample_id: sample.id,
            retention,
        })
    }

    pub fn append_outcome(&self, outcome: &OutcomeRecord) -> Result<(), FineTuneError> {
        let _guard = self.guard()?;
        self.append_json_locked("outcomes", outcome.recorded_at.month_key(), outcome)?;
        self.invalidate_cache();
        Ok(())
    }

    pub fn stats(&self) -> Result<Stats, FineTuneError> {
        let _guard = self.guard()?;
        let files = self.sample_files()?;
        let outcome_files = self.outcome_files()?;
        let signature = signature(files.iter().chain(outcome_files.iter()));
        if let Some(signature) = signature.as_ref() {
            let cache = self
                .stats_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((cached, stats)) = cache.as_ref()
                && cached == signature
            {
                return Ok(stats.clone());
            }
        }
        let (outcomes, unreadable_outcomes) = self.load_outcomes_locked()?;
        let mut stats = Stats {
            files: files.len(),
            outcome_files: outcome_files.len(),
            unreadable_outcomes,
            ..Stats::default()
        };
        for path in &files {
            for_each_line(path, |line| {
                match parse_line::<FineTuneSample>(line) {
                    Parsed::Empty => {}
                    Parsed::Invalid => stats.unreadable_lines += 1,
                    Parsed::Valid(sample) => {
                        stats.total += 1;
                        *stats
                            .by_character
                            .entry(sample.character_id.clone())
                            .or_default() += 1;
                        *stats
                            .by_model
                            .entry(sample.model.clone().unwrap_or_else(|| "unknown".into()))
                            .or_default() += 1;
                        match sample.rating {
                            Some(Rating::Positive) => {
                                stats.rated += 1;
                                stats.positive += 1;
                            }
                            Some(Rating::Negative) => {
                                stats.rated += 1;
                                stats.negative += 1;
                            }
                            None => {}
                        }
                        match outcomes.get(&sample.id).map(|record| record.outcome) {
                            Some(outcome) if outcome.is_accepted() => stats.accepted += 1,
                            Some(_) => stats.not_accepted += 1,
                            None => stats.outcome_unknown += 1,
                        }
                    }
                }
                Ok(())
            })?;
        }
        if let Some(signature) = signature {
            *self
                .stats_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some((signature, stats.clone()));
        }
        Ok(stats)
    }

    pub fn scan(&self, filters: &SampleFilters) -> Result<ScanSummary, FineTuneError> {
        let _guard = self.guard()?;
        let (outcomes, unreadable_outcomes) = self.load_outcomes_locked()?;
        let mut summary = ScanSummary {
            unreadable_outcomes,
            ..ScanSummary::default()
        };
        for path in self.sample_files()? {
            for_each_line(&path, |line| {
                match parse_line::<FineTuneSample>(line) {
                    Parsed::Empty => {}
                    Parsed::Invalid => summary.unreadable_lines += 1,
                    Parsed::Valid(sample) => {
                        if sample.matches(filters, outcomes.get(&sample.id)) {
                            summary.matched += 1;
                            summary.dropped_text_export_messages +=
                                sample.dropped_text_export_messages();
                            let parts = sample.non_text_parts();
                            if parts > 0 {
                                summary.multimodal_samples += 1;
                                summary.dropped_non_text_parts += parts;
                            }
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(summary)
    }

    pub fn list(
        &self,
        filters: &SampleFilters,
        limit: usize,
    ) -> Result<ListOutcome, FineTuneError> {
        let _guard = self.guard()?;
        let (outcomes, unreadable_outcomes) = self.load_outcomes_locked()?;
        let mut outcome = ListOutcome {
            rows: Vec::new(),
            matched: 0,
            unreadable_lines: 0,
            unreadable_outcomes,
        };
        for path in self.sample_files()? {
            for_each_line(&path, |line| {
                match parse_line::<FineTuneSample>(line) {
                    Parsed::Empty => {}
                    Parsed::Invalid => outcome.unreadable_lines += 1,
                    Parsed::Valid(sample) => {
                        let record = outcomes.get(&sample.id);
                        if sample.matches(filters, record) {
                            outcome.matched += 1;
                            if outcome.rows.len() < limit {
                                outcome.rows.push(sample.summary(record));
                            }
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(outcome)
    }

    pub fn set_rating(
        &self,
        sample_id: SampleId,
        rating: Rating,
    ) -> Result<RatingUpdate, FineTuneError> {
        let _guard = self.guard()?;
        let files = self.sample_files()?;
        for path in files.into_iter().rev() {
            if let Some(update) = self.rewrite_rating(&path, sample_id, rating)? {
                self.invalidate_cache();
                return Ok(update);
            }
        }
        Err(FineTuneError::SampleNotFound(sample_id))
    }

    pub fn enforce_retention(
        &self,
        retention: Retention,
    ) -> Result<RetentionOutcome, FineTuneError> {
        let _guard = self.guard()?;
        let outcome = self.enforce_retention_locked(retention)?;
        self.invalidate_cache();
        Ok(outcome)
    }

    pub fn export(
        &self,
        filters: &SampleFilters,
        format: ExportFormat,
        output: &Path,
    ) -> Result<ExportReport, FineTuneError> {
        let _guard = self.guard()?;
        if output.exists() {
            return Err(FineTuneError::DestinationExists(output.to_path_buf()));
        }
        let parent = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(FineTuneError::NotDirectory(parent.to_path_buf()));
        }
        if is_managed_name(output) {
            return Err(FineTuneError::ManagedOutput(output.to_path_buf()));
        }
        let canonical_root =
            fs::canonicalize(&*self.root).map_err(|error| io_error(&self.root, error))?;
        let canonical_parent = fs::canonicalize(parent).map_err(|error| io_error(parent, error))?;
        if canonical_parent == canonical_root {
            return Err(FineTuneError::ManagedOutput(output.to_path_buf()));
        }
        let (outcomes, unreadable_outcomes) = self.load_outcomes_locked()?;
        let temporary = temporary_path(parent, "finetune-export");
        let result = self.write_export(&temporary, filters, format, &outcomes, unreadable_outcomes);
        let mut report = match result {
            Ok(report) => report,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        if report.matched == 0 {
            let _ = fs::remove_file(&temporary);
            return Err(FineTuneError::NoMatchingSamples);
        }
        match fs::hard_link(&temporary, output) {
            Ok(()) => {
                let _ = fs::remove_file(&temporary);
                report.output = output.to_path_buf();
                Ok(report)
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                if error.kind() == io::ErrorKind::AlreadyExists {
                    Err(FineTuneError::DestinationExists(output.to_path_buf()))
                } else {
                    Err(io_error(output, error))
                }
            }
        }
    }

    pub fn clear(&self, _confirmation: ClearConfirmation) -> Result<ClearOutcome, FineTuneError> {
        let _guard = self.guard()?;
        let mut outcome = ClearOutcome::default();
        for path in self
            .sample_files()?
            .into_iter()
            .chain(self.outcome_files()?)
        {
            match fs::remove_file(&path) {
                Ok(()) => outcome.removed.push(path),
                Err(error) => outcome.failures.push(FileFailure {
                    path,
                    message: error.to_string(),
                }),
            }
        }
        self.invalidate_cache();
        Ok(outcome)
    }

    fn write_export(
        &self,
        path: &Path,
        filters: &SampleFilters,
        format: ExportFormat,
        outcomes: &BTreeMap<SampleId, OutcomeRecord>,
        unreadable_outcomes: usize,
    ) -> Result<ExportReport, FineTuneError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| io_error(path, error))?;
        let mut writer = BufWriter::new(file);
        let mut report = ExportReport {
            format,
            output: path.to_path_buf(),
            matched: 0,
            written: 0,
            unreadable_lines: 0,
            unreadable_outcomes,
            multimodal_samples: 0,
            dropped_non_text_parts: 0,
            dropped_text_export_messages: 0,
        };
        for sample_path in self.sample_files()? {
            for_each_line(&sample_path, |line| {
                let sample = match parse_line::<FineTuneSample>(line) {
                    Parsed::Empty => return Ok(()),
                    Parsed::Invalid => {
                        report.unreadable_lines += 1;
                        return Ok(());
                    }
                    Parsed::Valid(sample) => sample,
                };
                let outcome = outcomes.get(&sample.id);
                if !sample.matches(filters, outcome) {
                    return Ok(());
                }
                report.matched += 1;
                report.dropped_text_export_messages += sample.dropped_text_export_messages();
                let parts = sample.non_text_parts();
                if parts > 0 {
                    report.multimodal_samples += 1;
                    report.dropped_non_text_parts += parts;
                }
                let value = match format {
                    ExportFormat::RawJsonl => {
                        let mut value: Value =
                            serde_json::from_slice(trim_line(line)).map_err(|error| {
                                FineTuneError::SettingsCorrupt(format!(
                                    "{}: {error}",
                                    sample_path.display()
                                ))
                            })?;
                        value["accepted_outcome"] = outcome
                            .map(|record| json!(record.outcome))
                            .unwrap_or(Value::Null);
                        value
                    }
                    ExportFormat::ShareGpt => sample.sharegpt_value(),
                    ExportFormat::ChatMl => sample.chatml_value(),
                };
                serde_json::to_writer(&mut writer, &value).map_err(|error| FineTuneError::Io {
                    path: path.to_path_buf(),
                    source: io::Error::other(error.to_string()),
                })?;
                writer
                    .write_all(b"\n")
                    .map_err(|error| io_error(path, error))?;
                report.written += 1;
                Ok(())
            })?;
        }
        writer.flush().map_err(|error| io_error(path, error))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| io_error(path, error))?;
        Ok(report)
    }

    fn rewrite_rating(
        &self,
        path: &Path,
        sample_id: SampleId,
        rating: Rating,
    ) -> Result<Option<RatingUpdate>, FineTuneError> {
        let temporary = temporary_path(
            path.parent().unwrap_or_else(|| Path::new(".")),
            "finetune-rating",
        );
        let source = File::open(path).map_err(|error| io_error(path, error))?;
        let target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| io_error(&temporary, error))?;
        let mut reader = BufReader::new(source);
        let mut writer = BufWriter::new(target);
        let mut line = Vec::new();
        let mut found = false;
        let mut previous = None;
        loop {
            line.clear();
            let read = read_bounded_line(&mut reader, &mut line, path)?;
            if read == 0 {
                break;
            }
            if line.len() > MAX_RECORD_BYTES {
                cleanup_temporary(&temporary);
                return Err(record_too_long(path));
            }
            let payload = trim_line(&line);
            if !found
                && !payload.is_empty()
                && let Ok(mut value) = serde_json::from_slice::<Value>(payload)
            {
                let wanted = sample_id.to_string();
                if value.get("id").and_then(Value::as_str) == Some(wanted.as_str()) {
                    previous = value
                        .get("rating")
                        .and_then(Value::as_i64)
                        .and_then(|value| i8::try_from(value).ok())
                        .and_then(|value| Rating::try_from(value).ok());
                    value["rating"] = json!(rating.as_i8());
                    serde_json::to_writer(&mut writer, &value).map_err(|error| {
                        FineTuneError::Io {
                            path: temporary.clone(),
                            source: io::Error::other(error.to_string()),
                        }
                    })?;
                    writer
                        .write_all(b"\n")
                        .map_err(|error| io_error(&temporary, error))?;
                    found = true;
                    continue;
                }
            }
            writer
                .write_all(&line)
                .map_err(|error| io_error(&temporary, error))?;
        }
        writer
            .flush()
            .map_err(|error| io_error(&temporary, error))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| io_error(&temporary, error))?;
        drop(writer);
        if !found {
            cleanup_temporary(&temporary);
            return Ok(None);
        }
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(Some(RatingUpdate {
                previous,
                current: rating,
            })),
            Err(error) => {
                cleanup_temporary(&temporary);
                Err(io_error(path, error))
            }
        }
    }

    fn enforce_retention_locked(
        &self,
        retention: Retention,
    ) -> Result<RetentionOutcome, FineTuneError> {
        let Some(max) = retention.max_records() else {
            return Ok(RetentionOutcome::default());
        };
        let files = self.sample_files()?;
        let mut counts = Vec::new();
        let mut total = 0usize;
        for path in files {
            let mut valid = 0usize;
            let mut unreadable = 0usize;
            for_each_line(&path, |line| {
                match parse_line::<FineTuneSample>(line) {
                    Parsed::Empty => {}
                    Parsed::Valid(_) => valid += 1,
                    Parsed::Invalid => unreadable += 1,
                }
                Ok(())
            })?;
            total += valid;
            counts.push((path, valid, unreadable));
        }
        if total <= max {
            return Ok(RetentionOutcome {
                remaining_samples: total,
                ..RetentionOutcome::default()
            });
        }
        let mut to_drop = total - max;
        let mut removed_samples = 0usize;
        let mut removed_files = 0usize;
        let mut preserved_unreadable_lines = 0usize;
        let mut removed_ids = HashSet::new();
        for (path, valid, unreadable) in counts {
            if to_drop == 0 {
                break;
            }
            if valid == 0 {
                continue;
            }
            if unreadable == 0 && valid <= to_drop {
                let mut ids = Vec::new();
                for_each_line(&path, |line| {
                    if let Parsed::Valid(sample) = parse_line::<FineTuneSample>(line) {
                        ids.push(sample.id);
                    }
                    Ok(())
                })?;
                fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
                removed_ids.extend(ids);
                removed_samples += valid;
                removed_files += 1;
                to_drop -= valid;
                continue;
            }
            let drop = to_drop.min(valid);
            let rewritten = self.rewrite_dropping(&path, drop)?;
            removed_ids.extend(rewritten.removed_ids);
            removed_samples += drop;
            preserved_unreadable_lines += rewritten.unreadable;
            to_drop -= drop;
        }
        let removed_outcomes = self.prune_outcomes(&removed_ids)?;
        Ok(RetentionOutcome {
            removed_samples,
            remaining_samples: total - removed_samples,
            removed_files,
            removed_outcomes,
            preserved_unreadable_lines,
        })
    }

    fn rewrite_dropping(
        &self,
        path: &Path,
        drop_count: usize,
    ) -> Result<DropRewrite, FineTuneError> {
        let temporary = temporary_path(
            path.parent().unwrap_or_else(|| Path::new(".")),
            "finetune-retention",
        );
        let source = File::open(path).map_err(|error| io_error(path, error))?;
        let target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| io_error(&temporary, error))?;
        let mut reader = BufReader::new(source);
        let mut writer = BufWriter::new(target);
        let mut line = Vec::new();
        let mut removed_ids = Vec::new();
        let mut unreadable = 0usize;
        let mut written = 0usize;
        loop {
            line.clear();
            let read = read_bounded_line(&mut reader, &mut line, path)?;
            if read == 0 {
                break;
            }
            if line.len() > MAX_RECORD_BYTES {
                cleanup_temporary(&temporary);
                return Err(record_too_long(path));
            }
            if removed_ids.len() < drop_count {
                match parse_line::<FineTuneSample>(&line) {
                    Parsed::Valid(sample) => {
                        removed_ids.push(sample.id);
                        continue;
                    }
                    Parsed::Invalid => unreadable += 1,
                    Parsed::Empty => {}
                }
            } else if matches!(parse_line::<FineTuneSample>(&line), Parsed::Invalid) {
                unreadable += 1;
            }
            writer
                .write_all(&line)
                .map_err(|error| io_error(&temporary, error))?;
            written += 1;
        }
        writer
            .flush()
            .map_err(|error| io_error(&temporary, error))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| io_error(&temporary, error))?;
        drop(writer);
        if written == 0 {
            cleanup_temporary(&temporary);
            fs::remove_file(path).map_err(|error| io_error(path, error))?;
        } else {
            fs::rename(&temporary, path).map_err(|error| {
                cleanup_temporary(&temporary);
                io_error(path, error)
            })?;
        }
        Ok(DropRewrite {
            removed_ids,
            unreadable,
        })
    }

    fn prune_outcomes(&self, removed: &HashSet<SampleId>) -> Result<usize, FineTuneError> {
        if removed.is_empty() {
            return Ok(0);
        }
        let mut removed_outcomes = 0usize;
        for path in self.outcome_files()? {
            let temporary = temporary_path(
                path.parent().unwrap_or_else(|| Path::new(".")),
                "finetune-outcome-prune",
            );
            let source = File::open(&path).map_err(|error| io_error(&path, error))?;
            let target = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| io_error(&temporary, error))?;
            let mut reader = BufReader::new(source);
            let mut writer = BufWriter::new(target);
            let mut line = Vec::new();
            let mut kept = 0usize;
            loop {
                line.clear();
                let read = read_bounded_line(&mut reader, &mut line, &path)?;
                if read == 0 {
                    break;
                }
                if line.len() > MAX_RECORD_BYTES {
                    cleanup_temporary(&temporary);
                    return Err(record_too_long(&path));
                }
                if let Parsed::Valid(record) = parse_line::<OutcomeRecord>(&line)
                    && removed.contains(&record.sample_id)
                {
                    removed_outcomes += 1;
                    continue;
                }
                writer
                    .write_all(&line)
                    .map_err(|error| io_error(&temporary, error))?;
                kept += 1;
            }
            writer
                .flush()
                .map_err(|error| io_error(&temporary, error))?;
            writer
                .get_ref()
                .sync_all()
                .map_err(|error| io_error(&temporary, error))?;
            drop(writer);
            if kept == 0 {
                cleanup_temporary(&temporary);
                fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
            } else {
                fs::rename(&temporary, &path).map_err(|error| {
                    cleanup_temporary(&temporary);
                    io_error(&path, error)
                })?;
            }
        }
        Ok(removed_outcomes)
    }

    fn load_outcomes_locked(
        &self,
    ) -> Result<(BTreeMap<SampleId, OutcomeRecord>, usize), FineTuneError> {
        let mut outcomes = BTreeMap::new();
        let mut unreadable = 0usize;
        for path in self.outcome_files()? {
            for_each_line(&path, |line| {
                match parse_line::<OutcomeRecord>(line) {
                    Parsed::Empty => {}
                    Parsed::Invalid => unreadable += 1,
                    Parsed::Valid(record) => {
                        let replace = outcomes.get(&record.sample_id).is_none_or(
                            |current: &OutcomeRecord| current.recorded_at <= record.recorded_at,
                        );
                        if replace {
                            outcomes.insert(record.sample_id, record);
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok((outcomes, unreadable))
    }

    fn append_json_locked<T: serde::Serialize>(
        &self,
        prefix: &str,
        month: String,
        value: &T,
    ) -> Result<(), FineTuneError> {
        let path = self.root.join(format!("{prefix}_{month}.jsonl"));
        let separator = needs_separator(&path)?;
        let mut bytes = serde_json::to_vec(value).map_err(|error| {
            FineTuneError::SettingsInvalid(format!("cannot encode dataset record: {error}"))
        })?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(record_too_long(&path));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|error| io_error(&path, error))?;
        let original_len = file
            .metadata()
            .map_err(|error| io_error(&path, error))?
            .len();
        let write = (|| -> io::Result<()> {
            if separator {
                file.write_all(b"\n")?;
            }
            file.write_all(&bytes)?;
            file.sync_all()
        })();
        if let Err(error) = write {
            let rollback = file.set_len(original_len).and_then(|()| file.sync_all());
            if let Err(rollback) = rollback {
                return Err(FineTuneError::Io {
                    path,
                    source: io::Error::other(format!(
                        "append failed ({error}); rollback also failed ({rollback})"
                    )),
                });
            }
            return Err(io_error(&path, error));
        }
        Ok(())
    }

    fn sample_files(&self) -> Result<Vec<PathBuf>, FineTuneError> {
        self.files("samples_")
    }

    fn outcome_files(&self) -> Result<Vec<PathBuf>, FineTuneError> {
        self.files("outcomes_")
    }

    fn files(&self, prefix: &str) -> Result<Vec<PathBuf>, FineTuneError> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&*self.root).map_err(|error| io_error(&self.root, error))? {
            let entry = entry.map_err(|error| io_error(&self.root, error))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(key) = name
                .strip_prefix(prefix)
                .and_then(|value| value.strip_suffix(".jsonl"))
            else {
                continue;
            };
            if key.len() == 6
                && key.bytes().all(|byte| byte.is_ascii_digit())
                && key[4..]
                    .parse::<u32>()
                    .is_ok_and(|month| (1..=12).contains(&month))
            {
                files.push(entry.path());
            }
        }
        files.sort();
        Ok(files)
    }

    fn guard(&self) -> Result<StoreGuard<'_>, FineTuneError> {
        let deadline = Instant::now() + Duration::from_millis(250);
        let check = || {
            if self.cancellation.is_cancelled() || Instant::now() >= deadline {
                Err(io_error(
                    self.root(),
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "dataset lock cancelled or timed out",
                    ),
                ))
            } else {
                Ok(())
            }
        };
        let mutex = loop {
            check()?;
            match self.io_lock.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(2))
                }
            }
        };
        let path = self.root.join(".neuromita-finetune.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| io_error(&path, error))?;
        loop {
            check()?;
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(io_error(&path, error)),
            }
        }
        Ok(StoreGuard {
            _mutex: mutex,
            _lock: file,
        })
    }

    fn invalidate_cache(&self) {
        *self
            .stats_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

struct StoreGuard<'a> {
    _lock: File,
    _mutex: std::sync::MutexGuard<'a, ()>,
}

fn is_managed_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".jsonl") && (name.starts_with("samples_") || name.starts_with("outcomes_"))
}

enum Parsed<T> {
    Empty,
    Valid(T),
    Invalid,
}

fn parse_line<T: DeserializeOwned>(line: &[u8]) -> Parsed<T> {
    let payload = trim_line(line);
    if payload.iter().all(u8::is_ascii_whitespace) {
        return Parsed::Empty;
    }
    match serde_json::from_slice(payload) {
        Ok(value) => Parsed::Valid(value),
        Err(_) => Parsed::Invalid,
    }
}

fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn for_each_line(
    path: &Path,
    mut visit: impl FnMut(&[u8]) -> Result<(), FineTuneError>,
) -> Result<(), FineTuneError> {
    let source = File::open(path).map_err(|error| io_error(path, error))?;
    let mut reader = BufReader::new(source);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = read_bounded_line(&mut reader, &mut line, path)?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_RECORD_BYTES {
            return Err(record_too_long(path));
        }
        visit(&line)?;
    }
    Ok(())
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    path: &Path,
) -> Result<usize, FineTuneError> {
    reader
        .take((MAX_RECORD_BYTES + 1) as u64)
        .read_until(b'\n', line)
        .map_err(|error| io_error(path, error))
}

fn needs_separator(path: &Path) -> Result<bool, FineTuneError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error(path, error)),
    };
    if metadata.len() == 0 {
        return Ok(false);
    }
    let mut file = File::open(path).map_err(|error| io_error(path, error))?;
    file.seek(SeekFrom::End(-1))
        .map_err(|error| io_error(path, error))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)
        .map_err(|error| io_error(path, error))?;
    if last[0] == b'\n' {
        return Ok(false);
    }
    let start = metadata.len().saturating_sub(MAX_RECORD_BYTES as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| io_error(path, error))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)
        .map_err(|error| io_error(path, error))?;
    let last_line = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(tail.as_slice(), |index| &tail[index + 1..]);
    if last_line.iter().all(u8::is_ascii_whitespace) {
        return Ok(true);
    }
    if serde_json::from_slice::<Value>(last_line).is_ok() {
        Ok(true)
    } else {
        Err(FineTuneError::IncompleteTail(path.to_path_buf()))
    }
}

fn signature<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Option<Signature> {
    let mut signature = Vec::new();
    for path in paths {
        let metadata = fs::metadata(path).ok()?;
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        signature.push((
            path.to_string_lossy().into_owned(),
            modified,
            metadata.len(),
        ));
    }
    Some(signature)
}

fn temporary_path(directory: &Path, stem: &str) -> PathBuf {
    directory.join(format!(".{stem}.{}.tmp", Uuid::new_v4()))
}

fn cleanup_temporary(path: &Path) {
    let _ = fs::remove_file(path);
}

fn io_error(path: &Path, source: io::Error) -> FineTuneError {
    FineTuneError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn record_too_long(path: &Path) -> FineTuneError {
    FineTuneError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::InvalidData,
            format!("record exceeds {MAX_RECORD_BYTES} bytes"),
        ),
    }
}
