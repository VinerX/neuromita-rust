use std::{collections::BTreeSet, path::PathBuf};

use thiserror::Error;

use super::*;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    FineTune(#[from] FineTuneError),
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error("clear removed {removed} file(s) but failed for {failed}")]
    PartialClear { removed: usize, failed: usize },
}

pub fn run(args: Vec<String>) -> Result<(), CliError> {
    if args.is_empty()
        || args[0] == "help"
        || args[0] == "--help"
        || args.iter().any(|argument| argument == "--help")
    {
        help();
        return Ok(());
    }
    let mut parsed = Parsed::parse(args)?;
    let command = parsed
        .positional
        .first()
        .cloned()
        .ok_or_else(|| usage("dataset command is required; use dataset help"))?;
    parsed.positional.remove(0);
    match command.as_str() {
        "init" => init(&parsed),
        "config" => config(&parsed),
        "stats" => stats(&parsed),
        "list" => list(&parsed),
        "rate" => rate(&parsed),
        "export" => export(&parsed),
        "clear" => clear(&parsed),
        other => Err(usage(format!("unknown dataset command: {other}"))),
    }
}

fn init(parsed: &Parsed) -> Result<(), CliError> {
    parsed.allow(
        &["--config", "--dir", "--enabled", "--max"],
        &["--unlimited"],
    )?;
    parsed.expect_positional(0)?;
    let config = config_path(parsed)?;
    if config.exists() {
        return Err(usage(format!(
            "settings file already exists: {}",
            config.display()
        )));
    }
    let data_dir = PathBuf::from(
        parsed
            .single("--dir")?
            .ok_or_else(|| usage("--dir is required for init"))?,
    );
    if !data_dir.is_absolute() {
        return Err(FineTuneError::RelativeDirectory(data_dir).into());
    }
    let enabled = on_off(parsed.single("--enabled")?, "--enabled")?;
    let retention = retention(parsed)?;
    let settings = FineTuneSettings::new(data_dir.clone(), enabled, retention)?;
    FineTuneStore::open(data_dir)?;
    FineTuneSettings::create_new(&config, &settings)?;
    println!("Created {}", config.display());
    println!("Data directory: {}", settings.data_dir.display());
    println!(
        "Collection: {}; retention: {}",
        if settings.enabled {
            "enabled"
        } else {
            "disabled"
        },
        describe_retention(settings.retention)
    );
    Ok(())
}

fn config(parsed: &Parsed) -> Result<(), CliError> {
    parsed.allow(&["--config"], &[])?;
    let path = config_path(parsed)?;
    let subcommand = parsed
        .positional
        .first()
        .cloned()
        .ok_or_else(|| usage("config subcommand is required"))?;
    let mut settings = FineTuneSettings::load(&path)?;
    match subcommand.as_str() {
        "show" => {
            parsed.expect_positional(1)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&settings)
                    .map_err(|error| usage(error.to_string()))?
            );
            return Ok(());
        }
        "enable" => {
            parsed.expect_positional(2)?;
            settings.enabled = on_off(parsed.positional.get(1).cloned(), "config enable")?;
        }
        "max" => {
            parsed.expect_positional(2)?;
            settings.retention = match parsed.positional.get(1).map(String::as_str) {
                Some("unlimited") => Retention::Unlimited,
                Some(value) => Retention::limited(
                    value
                        .parse()
                        .map_err(|_| usage("config max expects a number or unlimited"))?,
                )?,
                None => return Err(usage("config max expects a number or unlimited")),
            };
        }
        "dir" => {
            parsed.expect_positional(2)?;
            settings.set_data_dir(PathBuf::from(
                parsed
                    .positional
                    .get(1)
                    .ok_or_else(|| usage("config dir expects a path"))?,
            ))?;
        }
        other => return Err(usage(format!("unknown config subcommand: {other}"))),
    }
    settings.save(&path)?;
    println!("Updated {}", path.display());
    if subcommand == "max" {
        let store = FineTuneStore::open(settings.data_dir.clone())?;
        let outcome = store.enforce_retention(settings.retention)?;
        println!(
            "Retention applied: removed {} sample(s), {} file(s), kept {}; preserved {} unreadable line(s).",
            outcome.removed_samples,
            outcome.removed_files,
            outcome.remaining_samples,
            outcome.preserved_unreadable_lines
        );
    }
    match subcommand.as_str() {
        "enable" | "dir" | "max" => println!(
            "Note: a running Core keeps its loaded collection, data directory and retention until restart; start Core again to apply the saved values."
        ),
        _ => {}
    }
    Ok(())
}

fn stats(parsed: &Parsed) -> Result<(), CliError> {
    parsed.allow(&["--config"], &[])?;
    parsed.expect_positional(0)?;
    let settings = load(parsed)?;
    let store = FineTuneStore::open(settings.data_dir.clone())?;
    let stats = store.stats()?;
    println!("Files: {} (outcomes: {})", stats.files, stats.outcome_files);
    println!("Total provider-response samples: {}", stats.total);
    println!(
        "Accepted: {}; not accepted: {}; outcome unknown: {}",
        stats.accepted, stats.not_accepted, stats.outcome_unknown
    );
    println!(
        "Rated: {} (positive: {}, negative: {})",
        stats.rated, stats.positive, stats.negative
    );
    println!(
        "Unreadable lines: {}; unreadable outcome lines: {}",
        stats.unreadable_lines, stats.unreadable_outcomes
    );
    println!("By character:");
    for (character, count) in stats.by_character {
        println!("  {character}: {count}");
    }
    println!("By model:");
    for (model, count) in stats.by_model {
        println!("  {model}: {count}");
    }
    Ok(())
}

fn list(parsed: &Parsed) -> Result<(), CliError> {
    parsed.allow(
        &[
            "--config",
            "--from",
            "--to",
            "--character",
            "--model",
            "--rating",
            "--outcome",
            "--limit",
        ],
        &[],
    )?;
    parsed.expect_positional(0)?;
    let settings = load(parsed)?;
    let store = FineTuneStore::open(settings.data_dir)?;
    let filters = filters(parsed)?;
    let limit = parsed
        .single("--limit")?
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| usage("--limit expects a number"))
        })
        .transpose()?
        .unwrap_or(50);
    let result = store.list(&filters, limit)?;
    for row in &result.rows {
        println!(
            "{} {} character={} model={} rating={} outcome={} request={}",
            row.timestamp.to_rfc3339(),
            row.id,
            row.character_id,
            row.model.as_deref().unwrap_or("-"),
            row.rating
                .map_or("unrated", |rating| if rating.as_i8() > 0 {
                    "+1"
                } else {
                    "-1"
                }),
            row.outcome.map(describe_outcome).unwrap_or("unknown"),
            row.request_id
                .map_or_else(|| "-".into(), |id| id.to_string())
        );
    }
    println!(
        "Matched: {}; shown: {}; unreadable lines: {}; unreadable outcome lines: {}",
        result.matched,
        result.rows.len(),
        result.unreadable_lines,
        result.unreadable_outcomes
    );
    Ok(())
}

fn rate(parsed: &Parsed) -> Result<(), CliError> {
    parsed.allow(&["--config", "--sample", "--value"], &[])?;
    parsed.expect_positional(0)?;
    let settings = load(parsed)?;
    let store = FineTuneStore::open(settings.data_dir)?;
    let sample = SampleId::parse(
        &parsed
            .single("--sample")?
            .ok_or_else(|| usage("--sample UUID is required"))?,
    )
    .map_err(|error| usage(format!("invalid sample UUID: {error}")))?;
    let rating = match parsed.single("--value")?.as_deref() {
        Some("1") => Rating::Positive,
        Some("-1") => Rating::Negative,
        _ => return Err(usage("--value must be 1 or -1")),
    };
    let update = store.set_rating(sample, rating)?;
    println!(
        "Rated {}: {} -> {}",
        sample,
        update
            .previous
            .map_or("unrated".into(), |rating| rating.as_i8().to_string()),
        rating.as_i8()
    );
    Ok(())
}

fn export(parsed: &Parsed) -> Result<(), CliError> {
    parsed.allow(
        &[
            "--config",
            "--out",
            "--format",
            "--from",
            "--to",
            "--character",
            "--model",
            "--rating",
            "--outcome",
        ],
        &["--preview"],
    )?;
    parsed.expect_positional(0)?;
    let settings = load(parsed)?;
    let store = FineTuneStore::open(settings.data_dir)?;
    let filters = filters(parsed)?;
    let format = ExportFormat::parse(
        &parsed
            .single("--format")?
            .ok_or_else(|| usage("--format raw|sharegpt|chatml is required"))?,
    )?;
    let preview = store.scan(&filters)?;
    println!(
        "Matched: {}; unreadable lines: {}; unreadable outcome lines: {}",
        preview.matched, preview.unreadable_lines, preview.unreadable_outcomes
    );
    if format != ExportFormat::RawJsonl
        && (preview.dropped_non_text_parts > 0 || preview.dropped_text_export_messages > 0)
    {
        println!(
            "Warning: {:?} keeps text conversation content only; {} non-text part(s) from {} sample(s) and {} unsupported message(s) are dropped. Use raw for full data.",
            format,
            preview.dropped_non_text_parts,
            preview.multimodal_samples,
            preview.dropped_text_export_messages
        );
    }
    if parsed.flags.contains("preview") {
        return Ok(());
    }
    if preview.matched == 0 {
        return Err(FineTuneError::NoMatchingSamples.into());
    }
    let output = PathBuf::from(
        parsed
            .single("--out")?
            .ok_or_else(|| usage("--out FILE is required unless --preview is used"))?,
    );
    let report = store.export(&filters, format, &output)?;
    println!(
        "Exported {} record(s) to {} (format {:?}).",
        report.written,
        report.output.display(),
        report.format
    );
    if report.format != ExportFormat::RawJsonl
        && (report.dropped_non_text_parts > 0 || report.dropped_text_export_messages > 0)
    {
        println!(
            "Warning: {:?} exported text conversation content only; {} non-text part(s) from {} sample(s) and {} unsupported message(s) were dropped from this file. Use raw for full data.",
            report.format,
            report.dropped_non_text_parts,
            report.multimodal_samples,
            report.dropped_text_export_messages
        );
    }
    if report.unreadable_lines > 0 || report.unreadable_outcomes > 0 {
        println!(
            "Skipped unreadable data: {} sample line(s), {} outcome line(s).",
            report.unreadable_lines, report.unreadable_outcomes
        );
    }
    Ok(())
}

fn clear(parsed: &Parsed) -> Result<(), CliError> {
    parsed.allow(&["--config"], &["--yes"])?;
    parsed.expect_positional(0)?;
    if !parsed.flags.contains("yes") {
        return Err(FineTuneError::ConfirmationRequired.into());
    }
    let settings = load(parsed)?;
    let store = FineTuneStore::open(settings.data_dir)?;
    let outcome = store.clear(ClearConfirmation::confirmed())?;
    println!("Removed {} file(s).", outcome.removed.len());
    if !outcome.failures.is_empty() {
        for failure in &outcome.failures {
            eprintln!("Failed {}: {}", failure.path.display(), failure.message);
        }
        return Err(CliError::PartialClear {
            removed: outcome.removed.len(),
            failed: outcome.failures.len(),
        });
    }
    Ok(())
}

fn load(parsed: &Parsed) -> Result<FineTuneSettings, CliError> {
    Ok(FineTuneSettings::load(&config_path(parsed)?)?)
}

fn config_path(parsed: &Parsed) -> Result<PathBuf, CliError> {
    Ok(PathBuf::from(
        parsed
            .single("--config")?
            .ok_or_else(|| usage("--config PATH is required"))?,
    ))
}

fn filters(parsed: &Parsed) -> Result<SampleFilters, CliError> {
    let mut start = None;
    let mut end = None;
    if let Some(value) = parsed.single("--from")? {
        start = Some(time_bound(&value, false)?);
    }
    if let Some(value) = parsed.single("--to")? {
        end = Some(time_bound(&value, true)?);
    }
    let rating = match parsed.single("--rating")?.as_deref() {
        None | Some("all") => RatingFilter::All,
        Some("no-negative") => RatingFilter::NoNegative,
        Some("positive") => RatingFilter::PositiveOnly,
        Some(_) => return Err(usage("--rating must be all, no-negative or positive")),
    };
    let outcome = match parsed.single("--outcome")?.as_deref() {
        None | Some("all") => OutcomeFilter::All,
        Some("accepted") => OutcomeFilter::Accepted,
        Some("not-accepted") => OutcomeFilter::NotAccepted,
        Some(_) => return Err(usage("--outcome must be all, accepted or not-accepted")),
    };
    Ok(SampleFilters {
        range: TimeRange::new(start, end)?,
        characters: parsed.all("--character"),
        models: parsed.all("--model"),
        rating,
        outcome,
    })
}

fn time_bound(value: &str, end_of_day: bool) -> Result<UtcTimestamp, CliError> {
    if value.len() == 10 {
        let (start, end) = UtcTimestamp::utc_date_bounds(value)?;
        return Ok(if end_of_day { end } else { start });
    }
    Ok(UtcTimestamp::parse(value)?)
}

fn retention(parsed: &Parsed) -> Result<Retention, CliError> {
    match (parsed.single("--max")?, parsed.flags.contains("unlimited")) {
        (Some(_), true) => Err(usage("use either --max or --unlimited")),
        (None, false) => Err(usage("--max N or --unlimited is required")),
        (None, true) => Ok(Retention::Unlimited),
        (Some(value), false) => Ok(Retention::limited(
            value
                .parse()
                .map_err(|_| usage("--max expects a positive number"))?,
        )?),
    }
}

fn on_off(value: Option<String>, flag: &str) -> Result<bool, CliError> {
    match value.as_deref() {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err(usage(format!("{flag} must be on or off"))),
    }
}

fn describe_retention(retention: Retention) -> String {
    match retention {
        Retention::Unlimited => "unlimited".into(),
        Retention::Limited { max } => format!("last {max}"),
    }
}

fn describe_outcome(outcome: SampleOutcome) -> &'static str {
    match outcome {
        SampleOutcome::Accepted => "accepted",
        SampleOutcome::AcceptedVoiceFailed => "accepted_voice_failed",
        SampleOutcome::AcceptanceFailed => "acceptance_failed",
        SampleOutcome::CancelledBeforeAccept => "cancelled_before_accept",
    }
}

fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

fn help() {
    println!(
        "Neuromita dataset management (no provider credentials or network required)\n\n\
         neuromita dataset init --config PATH --dir ABS_PATH --enabled on|off --max N|--unlimited\n\
         neuromita dataset config --config PATH show|enable on|off|max N|unlimited|dir ABS_PATH\n\
         neuromita dataset stats --config PATH\n\
         neuromita dataset list --config PATH [--from DATE|RFC3339] [--to DATE|RFC3339] [--character ID]... [--model NAME]... [--rating all|no-negative|positive] [--outcome all|accepted|not-accepted] [--limit N]\n\
         neuromita dataset rate --config PATH --sample UUID --value 1|-1\n\
         neuromita dataset export --config PATH --format raw|sharegpt|chatml (--out FILE | --preview) [filters]\n\
         neuromita dataset clear --config PATH --yes\n\n\
         Date-only bounds cover the whole UTC day; RFC3339 bounds are exact and offsets are normalized to UTC. Ranges are inclusive. `config max` applies the new cap to existing files immediately, but a running Core keeps its loaded collection, data directory and retention until restart. `clear` removes samples and correlated outcomes; it never touches settings or user files outside sample/outcome JSONL. No command uploads data.\n\
         Raw export keeps all fields and adds `accepted_outcome`. ShareGPT/ChatML keep text parts only and print a loss warning."
    );
}

struct Parsed {
    flags: BTreeSet<String>,
    values: Vec<(String, String)>,
    positional: Vec<String>,
}

impl Parsed {
    fn parse(args: Vec<String>) -> Result<Self, CliError> {
        let mut parsed = Self {
            flags: BTreeSet::new(),
            values: Vec::new(),
            positional: Vec::new(),
        };
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            if let Some(flag) = argument.strip_prefix("--") {
                if matches!(flag, "help" | "yes" | "unlimited" | "preview") {
                    parsed.flags.insert(flag.to_owned());
                } else {
                    let value = args
                        .next()
                        .ok_or_else(|| usage(format!("--{flag} requires a value")))?;
                    parsed.values.push((format!("--{flag}"), value));
                }
            } else {
                parsed.positional.push(argument);
            }
        }
        if parsed.flags.contains("help") {
            help();
            return Err(CliError::Usage(String::new()));
        }
        Ok(parsed)
    }

    fn allow(&self, values: &[&str], flags: &[&str]) -> Result<(), CliError> {
        for (name, _) in &self.values {
            if !values.contains(&name.as_str()) {
                return Err(usage(format!("unknown option {name}")));
            }
        }
        for name in &self.flags {
            if !flags
                .iter()
                .any(|allowed| allowed.trim_start_matches('-') == name.as_str())
            {
                return Err(usage(format!("unknown option --{name}")));
            }
        }
        Ok(())
    }

    fn single(&self, name: &str) -> Result<Option<String>, CliError> {
        let mut values = self
            .values
            .iter()
            .filter(|(flag, _)| flag == name)
            .map(|(_, value)| value.clone());
        let value = values.next();
        if values.next().is_some() {
            return Err(usage(format!("{name} may be specified only once")));
        }
        Ok(value)
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(flag, _)| flag == name)
            .map(|(_, value)| value.clone())
            .collect()
    }

    fn expect_positional(&self, count: usize) -> Result<(), CliError> {
        if self.positional.len() != count {
            return Err(usage(format!(
                "expected {count} positional argument(s), got {}",
                self.positional.len()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_requires_confirmation_and_unknown_options_are_rejected() {
        assert!(matches!(
            run(vec![
                "clear".into(),
                "--config".into(),
                "missing.json".into()
            ]),
            Err(CliError::FineTune(FineTuneError::ConfirmationRequired))
        ));
        assert!(matches!(
            run(vec!["list".into(), "--bogus".into(), "x".into()]),
            Err(CliError::Usage(_))
        ));
    }
}
