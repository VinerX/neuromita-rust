use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

use neuromita::{
    api::ChatApi,
    character::CharacterId,
    games::{
        GameApi, GameReceipt,
        seabattle::{Coordinate, Orientation, Side},
    },
    presentation::ChatPresenter,
    request::{
        DiagnosticScope, DiagnosticSection, RequestId, RequestOutcome, RequestSnapshot,
        RequestSource, RequestState,
    },
};
use tokio::sync::mpsc;

pub struct Options {
    pub model: String,
    character: CharacterId,
    message: Option<String>,
    pub listen: std::net::SocketAddr,
    pub data_dir: PathBuf,
    pub finetune_config: Option<PathBuf>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl Options {
    pub fn parse() -> io::Result<Option<Self>> {
        let mut args = std::env::args().skip(1);
        let mut values = HashMap::new();
        while let Some(argument) = args.next() {
            if argument == "--help" {
                println!(
                    "neuromita --model MODEL --character Crazy|Kind --listen HOST:PORT [--message TEXT] [--data-dir PATH] [--finetune-config PATH]\n\nRequired environment: GEMINI_API_KEY\nNetwork command: test followed by newline\nInteractive commands: /cancel REQUEST_UUID, /character Crazy|Kind, /diagnostics ..., /dataset status, /game start|place|random|fire|turn|auto|close|view, /reminders, /reminder add|timer|update|delete|enable|disable|retry|zone|now ..., /quit\n\n{}",
                    crate::settings_cli::HELP
                );
                return Ok(None);
            }
            if !matches!(
                argument.as_str(),
                "--model"
                    | "--character"
                    | "--message"
                    | "--listen"
                    | "--data-dir"
                    | "--finetune-config"
            ) {
                return Err(invalid("unknown argument; use --help"));
            }
            let value = args
                .next()
                .ok_or_else(|| invalid("argument requires a value"))?;
            if values.insert(argument, value).is_some() {
                return Err(invalid("duplicate argument"));
            }
        }
        let model = values
            .remove("--model")
            .ok_or_else(|| invalid("--model is required"))?;
        let character = values
            .remove("--character")
            .ok_or_else(|| invalid("--character is required"))?;
        let character = CharacterId::parse(&character)
            .ok_or_else(|| invalid("unknown character; use a canonical character ID"))?;
        let listen = values
            .remove("--listen")
            .ok_or_else(|| invalid("--listen is required"))?
            .parse()
            .map_err(|_| invalid("invalid listen address"))?;
        Ok(Some(Self {
            model,
            character,
            message: values.remove("--message"),
            listen,
            data_dir: match values.remove("--data-dir") {
                Some(path) if path.is_empty() => return Err(invalid("empty data directory")),
                Some(path) => PathBuf::from(path),
                None => default_data_dir()?,
            },
            finetune_config: match values.remove("--finetune-config") {
                Some(path) if path.is_empty() => {
                    return Err(invalid("empty fine-tune configuration path"));
                }
                Some(path) => Some(PathBuf::from(path)),
                None => None,
            },
        }))
    }
}

fn default_data_dir() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("NEUROMITA_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(path).join("NeuroMita"));
    }
    #[cfg(not(windows))]
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("neuromita"));
    }
    if let Some(path) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(path).join(".config").join("neuromita"));
    }
    Err(invalid("data directory unavailable; specify --data-dir"))
}

fn input() -> mpsc::Receiver<io::Result<String>> {
    let (send, receive) = mpsc::channel(16);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if send.blocking_send(line).is_err() {
                break;
            }
        }
    });
    receive
}

/// Real user workflow over the same typed `RemindersApi` the model tools use.
async fn reminder_command(character: CharacterId, line: &str) -> Result<String, String> {
    use neuromita::reminders::{
        CreateReminder, CreateTimer, RemindersApi, TimeZoneId, UpdateReminder, parse_friendly_due,
        parse_structured_repeat, system_timezone,
    };
    let settings =
        neuromita::settings::SettingsApi::snapshot::<neuromita::settings::RemindersSettings>()
            .map_err(|error| error.to_string())?;
    let zone = settings.value.timezone.clone();
    let policy = settings.value.policy;
    let now = RemindersApi::now()
        .await
        .map_err(|error| error.to_string())?;
    let rest = if line.trim() == "/reminders" {
        "list"
    } else {
        line.trim_start_matches("/reminder").trim()
    };
    let (action, args) = match rest.split_once(char::is_whitespace) {
        Some((action, args)) => (action, args.trim()),
        None => (rest, ""),
    };
    match action {
        "" | "list" => {
            let views = RemindersApi::list(character)
                .await
                .map_err(|error| error.to_string())?;
            let mut lines = views.iter().map(|view| view.summary()).collect::<Vec<_>>();
            if lines.is_empty() {
                lines.push("Нет активных напоминаний.".into());
            }
            let snapshot = RemindersApi::snapshot().map_err(|error| error.to_string())?;
            for delivery in snapshot
                .deliveries
                .iter()
                .filter(|delivery| delivery.character == character)
            {
                lines.push(format!(
                    "Доставка {}: {:?}; запрос {:?}; ошибка {:?}",
                    delivery.id, delivery.state, delivery.request_id, delivery.last_error
                ));
            }
            Ok(lines.join("\n"))
        }
        "add" => {
            let (due, text) = args
                .split_once('|')
                .ok_or("формат: /reminder add <когда> | <текст>")?;
            let due = parse_friendly_due(due.trim(), now, &zone, policy)
                .map_err(|error| error.to_string())?;
            let view = RemindersApi::create(CreateReminder::new(character, text.trim(), due))
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Создано: {}", view.summary()))
        }
        "timer" => {
            let (delay, instruction) = args
                .split_once('|')
                .ok_or("формат: /reminder timer <секунды> | <инструкция>")?;
            let delay: f64 = delay
                .trim()
                .parse()
                .map_err(|_| "задержка должна быть числом секунд".to_string())?;
            if !delay.is_finite() || delay <= 0.0 {
                return Err("задержка должна быть положительной".into());
            }
            let delay = std::time::Duration::try_from_secs_f64(delay)
                .map_err(|_| "задержка вне допустимого диапазона".to_string())?;
            let view =
                RemindersApi::create_timer(CreateTimer::new(character, instruction.trim(), delay))
                    .await
                    .map_err(|error| error.to_string())?;
            Ok(format!("Запущен таймер: {}", view.summary()))
        }
        "update" => {
            let (number, rest) = args
                .split_once(char::is_whitespace)
                .ok_or("формат: /reminder update <N> text|due|repeat <значение>")?;
            let number: u64 = number
                .parse()
                .map_err(|_| "N должно быть числом".to_string())?;
            let (field, value) = rest
                .split_once(char::is_whitespace)
                .ok_or("формат: /reminder update <N> text|due|repeat <значение>")?;
            let mut input = UpdateReminder::new(character, number);
            match field {
                "text" => input = input.text(value.trim()),
                "due" => {
                    let due = parse_friendly_due(value.trim(), now, &zone, policy)
                        .map_err(|error| error.to_string())?;
                    input = input.due(due);
                }
                "repeat" => {
                    let repeat =
                        parse_structured_repeat(value.trim()).map_err(|error| error.to_string())?;
                    input = input.repeat(repeat);
                }
                other => return Err(format!("неизвестное поле '{other}'")),
            }
            let view = RemindersApi::update(input)
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Обновлено: {}", view.summary()))
        }
        "delete" => {
            let number: u64 = args
                .parse()
                .map_err(|_| "формат: /reminder delete <N>".to_string())?;
            let view = RemindersApi::delete(character, number)
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Удалено: {}", view.summary()))
        }
        "enable" | "disable" => {
            RemindersApi::set_enabled(action == "enable")
                .await
                .map_err(|error| error.to_string())?;
            Ok(if action == "enable" {
                "Планировщик напоминаний включён".into()
            } else {
                "Планировщик напоминаний выключен; записи сохранены".into()
            })
        }
        "retry" => {
            let id = args
                .parse::<uuid::Uuid>()
                .map_err(|_| "формат: /reminder retry <delivery-uuid>".to_string())?;
            let delivery = neuromita::reminders::DeliveryId(neuromita::timeline::EventId(id));
            RemindersApi::retry(delivery)
                .await
                .map_err(|error| error.to_string())?;
            Ok("Повтор доставки разрешён".into())
        }
        "zone" => {
            let zone = if args.eq_ignore_ascii_case("local") {
                system_timezone().map_err(|error| error.to_string())?
            } else {
                TimeZoneId::parse(args).map_err(|error| error.to_string())?
            };
            RemindersApi::set_timezone(zone.clone())
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Часовой пояс напоминаний: {zone}"))
        }
        "now" => Ok(format!("Сейчас: {now}")),
        other => Err(format!("неизвестная команда /reminder {other}")),
    }
}

fn display(snapshot: &RequestSnapshot) {
    let id = snapshot.metadata.id;
    let character = snapshot.metadata.character_id;
    match &snapshot.state {
        RequestState::Pending => println!("[{id}] {character}: pending"),
        RequestState::Processing(_) => println!("[{id}] {character}: processing"),
        RequestState::Cancelled => println!("[{id}] {character}: cancelled"),
        RequestState::Finished(RequestOutcome::Success(response)) => {
            println!("[{id}] {character}: success\n{}", response.text)
        }
        RequestState::Finished(RequestOutcome::TextGenerationFailed(error)) => {
            eprintln!("[{id}] {character}: text generation failed: {error}")
        }
        RequestState::Finished(RequestOutcome::InternalError(error)) => {
            eprintln!("[{id}] {character}: internal error: {error}")
        }
        RequestState::Finished(RequestOutcome::VoiceGenerationFailed { response, error }) => {
            eprintln!(
                "[{id}] {character}: voice failed: {error}\n{}",
                response.text
            );
        }
    }
    if snapshot.state.is_terminal() {
        if let Some(sample_id) = snapshot.dataset.sample_id {
            println!("[{id}] dataset sample: {sample_id}");
        }
        if let Some(warning) = &snapshot.dataset.warning {
            eprintln!("[{id}] dataset warning: {warning}");
        }
    }
}

fn dataset_status(dataset: Option<&neuromita::finetune::FineTuneCollector>) -> String {
    let Some(dataset) = dataset else {
        return "Dataset collection is not configured".into();
    };
    let status = dataset.delivery_status();
    format!(
        "Dataset enabled={} closed={} admitted={} completed={} pending={} dropped={} rejected={} errors={}\nLast delivery error: {}",
        dataset.enabled(),
        status.closed,
        status.admitted,
        status.completed,
        status.pending,
        status.dropped,
        status.rejected,
        status.errors,
        status.last_error.as_deref().unwrap_or("none")
    )
}

pub async fn run(
    presenter: &ChatPresenter,
    options: Options,
    dataset: Option<&neuromita::finetune::FineTuneCollector>,
) -> io::Result<()> {
    let mut character = options.character;
    let mut updates = presenter.observe();
    let mut printed: HashMap<RequestId, u64> = HashMap::new();
    let mut pending = HashSet::new();
    let mut failed = false;
    let mut eof = options.message.is_some();
    let mut lines = if eof { None } else { Some(input()) };
    if let Some(message) = options.message {
        let handle = presenter
            .reply(character, message)
            .map_err(|error| invalid(&error.to_string()))?;
        pending.insert(handle.id());
    } else {
        println!(
            "Gemini model: {}. Character: {character}. Enter a message; /quit to exit.",
            options.model
        );
    }
    loop {
        let model = { Arc::clone(&updates.borrow_and_update()) };
        if let Some(error) = &model.discovery_error {
            eprintln!("Observation warning: {error}");
        }
        for row in &model.requests {
            let id = row.metadata.id;
            if printed
                .get(&id)
                .is_none_or(|revision| *revision < row.snapshot.revision)
            {
                display(&row.snapshot);
                printed.insert(id, row.snapshot.revision);
            }
            if row.snapshot.state.is_terminal()
                && pending.remove(&id)
                && !matches!(
                    row.snapshot.state,
                    RequestState::Finished(RequestOutcome::Success(_))
                )
            {
                failed = true;
            }
            if let Some(error) = &row.observation_error {
                return Err(io::Error::other(error.to_string()));
            }
        }
        let observed_active = model
            .requests
            .iter()
            .any(|row| !row.snapshot.state.is_terminal());
        if eof && pending.is_empty() && !observed_active {
            return if failed {
                Err(io::Error::other("one or more requests did not succeed"))
            } else {
                Ok(())
            };
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; return Ok(()); }
            changed = updates.changed() => {
                changed.map_err(|_| io::Error::other("presenter stopped"))?;
            }
            line = async { lines.as_mut().expect("input enabled").recv().await }, if lines.is_some() => {
                match line {
                    None => { eof = true; lines = None; }
                    Some(Err(error)) => return Err(error),
                    Some(Ok(line)) => {
                        if line == "/quit" { return Ok(()); }
                        if let Some(value) = line.strip_prefix("/character ") {
                            match CharacterId::parse(value) {
                                Some(id) => { character = id; println!("Character: {id}"); }
                                None => eprintln!("Unknown character"),
                            }
                        } else if let Some(value) = line.strip_prefix("/cancel ") {
                            match uuid::Uuid::parse_str(value) {
                                Ok(id) => match presenter.cancel(RequestId(id)) {
                                    Ok(result) => println!("Cancel: {result:?}"),
                                    Err(error) => eprintln!("{error}"),
                                }
                                Err(_) => eprintln!("Invalid request UUID"),
                            }
                        } else if line == "/dataset status" {
                            println!("{}", dataset_status(dataset));
                        } else if let Some(value) = line.strip_prefix("/diagnostics") {
                            if let Err(error) = diagnostics_command(character, value.trim()) {
                                eprintln!("{error}");
                            }
                        } else if line == "/game" || line.starts_with("/game ") {
                            let value = line.strip_prefix("/game").unwrap_or("");
                            if let Some(id) = game_command(character, value.trim()).await {
                                pending.insert(id);
                            }
                        } else if line.starts_with("/reminder") {
                            match reminder_command(character, line.trim()).await {
                                Ok(text) => println!("{text}"),
                                Err(error) => eprintln!("{error}"),
                            }
                        } else {
                            match presenter.reply(character, line) {
                                Ok(handle) => { println!("Accepted: {}", handle.id()); pending.insert(handle.id()); }
                                Err(error) => eprintln!("{error}"),
                            }
                        }
                    }
                }
            }
        }
    }
}

fn diagnostics_command(character: CharacterId, arguments: &str) -> io::Result<()> {
    let access =
        ChatApi::diagnostics_for(DiagnosticScope::owner(character, RequestSource::Presenter))
            .map_err(|error| io::Error::other(error.to_string()))?;
    let mut parts = Arguments::new(arguments);
    match parts.next() {
        None | Some("help") => {
            println!(
                "/diagnostics latest [section]              exact latest trace in this scope\n\
                 /diagnostics list [limit]                  recent trace summaries\n\
                 /diagnostics errors [limit]                bounded error history\n\
                 /diagnostics ID [section]                  exact trace; no fallback\n\
                 /diagnostics view ID [section]             print JSON for copying\n\
                 /diagnostics export ID [section] PATH      write section JSON to PATH\n\
                 sections: all|ingress|prompt|request|response|usage|error|lifecycle"
            );
            Ok(())
        }
        Some("latest") => {
            let section = section(parts.next())?;
            let record = access
                .latest()
                .map_err(|error| io::Error::other(error.to_string()))?;
            println!(
                "{}",
                record
                    .export(section)
                    .map_err(|error| io::Error::other(error.to_string()))?
            );
            Ok(())
        }
        Some("list") => {
            let limit = limit(parts.next())?;
            let rows = access
                .recent(limit)
                .map_err(|error| io::Error::other(error.to_string()))?;
            for row in rows {
                println!(
                    "{} {} {:?} {:?} {}ms",
                    row.request_id,
                    row.character,
                    row.outcome.unwrap_or("incomplete"),
                    row.source,
                    row.elapsed_ms
                );
            }
            Ok(())
        }
        Some("errors") => {
            let limit = limit(parts.next())?;
            for error in access
                .errors(limit)
                .map_err(|error| io::Error::other(error.to_string()))?
            {
                println!(
                    "{} {} {:?} {:?} {}{}",
                    error.request_id,
                    error.character,
                    error.stage,
                    error.source,
                    error.message.as_str(),
                    if error.malformed { " (malformed)" } else { "" }
                );
            }
            Ok(())
        }
        Some("view") => print_exact(&access, parts.next(), parts.next()),
        Some("export") => {
            let id = parse_id(parts.next())?;
            let section = section(parts.next())?;
            let path = unquote(parts.rest());
            if path.is_empty() {
                return Err(invalid("export requires a path"));
            }
            let record = access
                .request(id)
                .map_err(|error| io::Error::other(error.to_string()))?;
            let text = record
                .export(section)
                .map_err(|error| io::Error::other(error.to_string()))?;
            write_export(path, &text)?;
            println!(
                "Exported {section} for {id} to {path} ({} bytes)",
                text.len()
            );
            Ok(())
        }
        Some(id) => {
            let id = parse_id(Some(id))?;
            let section = section(parts.next())?;
            let record = access
                .request(id)
                .map_err(|error| io::Error::other(error.to_string()))?;
            println!(
                "{}",
                record
                    .copy_text(section)
                    .map_err(|error| io::Error::other(error.to_string()))?
            );
            Ok(())
        }
    }
}

fn print_exact(
    access: &neuromita::request::DiagnosticsAccess,
    id: Option<&str>,
    section_value: Option<&str>,
) -> io::Result<()> {
    let id = parse_id(id)?;
    let section = section(section_value)?;
    let record = access
        .request(id)
        .map_err(|error| io::Error::other(error.to_string()))?;
    println!(
        "{}",
        record
            .copy_text(section)
            .map_err(|error| io::Error::other(error.to_string()))?
    );
    Ok(())
}

fn parse_id(value: Option<&str>) -> io::Result<RequestId> {
    value
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(RequestId)
        .ok_or_else(|| invalid("a request UUID is required"))
}

fn section(value: Option<&str>) -> io::Result<DiagnosticSection> {
    match value {
        None => Ok(DiagnosticSection::All),
        Some(value) => DiagnosticSection::parse(value).ok_or_else(|| {
            invalid("unknown diagnostic section; use all|ingress|prompt|request|response|usage|error|lifecycle")
        }),
    }
}

fn limit(value: Option<&str>) -> io::Result<usize> {
    match value {
        None => Ok(16),
        Some(value) => value
            .parse::<usize>()
            .map(|limit| limit.clamp(1, 64))
            .map_err(|_| invalid("limit must be a positive integer")),
    }
}

fn write_export(path: &str, text: &str) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

struct Arguments<'a> {
    value: &'a str,
    offset: usize,
}

impl<'a> Arguments<'a> {
    fn new(value: &'a str) -> Self {
        Self { value, offset: 0 }
    }

    fn next(&mut self) -> Option<&'a str> {
        let rest = self.value[self.offset..].trim_start();
        if rest.is_empty() {
            return None;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.offset = self.value.len() - rest.len() + end;
        Some(&rest[..end])
    }

    fn rest(&mut self) -> &'a str {
        let rest = self.value[self.offset..].trim_start();
        self.offset = self.value.len();
        rest
    }
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_never_overwrites_and_paths_may_contain_spaces() {
        let directory =
            std::env::temp_dir().join(format!("neuromita-terminal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let existing = directory.join("existing.json");
        std::fs::write(&existing, b"keep").unwrap();
        assert!(write_export(existing.to_str().unwrap(), "replace").is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep");

        let spaced = directory.join("trace with space.json");
        write_export(spaced.to_str().unwrap(), "{}").unwrap();
        assert_eq!(std::fs::read(&spaced).unwrap(), b"{}");

        let mut arguments = Arguments::new("request usage \"C:/tmp/trace with space.json\"");
        assert_eq!(arguments.next(), Some("request"));
        assert_eq!(arguments.next(), Some("usage"));
        assert_eq!(unquote(arguments.rest()), "C:/tmp/trace with space.json");
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod dataset_delivery_tests {
    use super::*;
    use neuromita::finetune::{
        FineTuneCollector, FineTuneSettings, Retention, SampleId, SampleOutcome,
    };
    #[tokio::test]
    async fn runtime_dataset_status_reports_late_writer_failure() {
        let root = std::env::temp_dir().join(format!("pr67-status-{}", uuid::Uuid::new_v4()));
        let collector = FineTuneCollector::from_settings(
            &FineTuneSettings::new(root.clone(), true, Retention::Unlimited).unwrap(),
        )
        .unwrap();
        std::fs::create_dir(root.join(".neuromita-finetune.lock")).unwrap();
        let request = RequestId(uuid::Uuid::new_v4());
        collector
            .enqueue_outcome(
                SampleId::for_request(request),
                request,
                None,
                SampleOutcome::Accepted,
            )
            .unwrap();
        collector
            .drain(std::time::Duration::from_secs(2), true)
            .await;
        let output = dataset_status(Some(&collector));
        assert!(output.contains("errors=1"), "{output}");
        assert!(output.contains("pending=0"), "{output}");
        assert!(output.contains(".neuromita-finetune.lock"), "{output}");
        drop(collector);
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn print_board(label: &str, board: &[String]) {
    println!("{label}");
    println!(
        "  {}",
        ('A'..='J')
            .map(|letter| letter.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    for (index, row) in board.iter().enumerate() {
        println!(
            "{:<2}{}",
            index + 1,
            row.chars()
                .map(|cell| cell.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}

fn show_game(receipt: &GameReceipt) {
    match (&receipt.view, receipt.key) {
        (Some(view), Some(key)) => {
            println!(
                "Sea Battle: {} | {} | сессия {} rev {} | авто: {}",
                view.phase.code(),
                view.status_line_for(Side::Player),
                key.session,
                key.revision,
                receipt.auto_reactions
            );
            if !view.remaining.is_empty() {
                println!(
                    "Корабли к расстановке: {}",
                    view.remaining
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
            print_board("Твоё поле:", &view.own_board);
            print_board("Поле противника:", &view.opponent_board);
            if let Some(last) = view.last_shot {
                println!(
                    "Последний выстрел: {} по {} — {}.",
                    last.attacker.label(),
                    last.coord,
                    last.result.code()
                );
            }
            if let Some(winner) = view.winner {
                let winner = if winner == Side::Player {
                    "игрок"
                } else {
                    "Мита"
                };
                println!("Победитель: {winner}");
            }
        }
        _ => println!("Активной партии нет."),
    }
}

fn parse_coord(value: Option<&str>) -> Result<Coordinate, String> {
    value
        .and_then(Coordinate::parse)
        .ok_or_else(|| "координата должна быть в виде A1..J10".to_owned())
}

fn no_extra_args(parts: &mut std::str::SplitWhitespace, usage: &str) -> bool {
    if parts.next().is_some() {
        eprintln!("usage: {usage}");
        false
    } else {
        true
    }
}

async fn game_command(character: CharacterId, rest: &str) -> Option<RequestId> {
    let mut parts = rest.split_whitespace();
    let command = parts.next().unwrap_or("view");
    let result = match command {
        "start" => {
            let auto = match parts.next() {
                None | Some("auto") => true,
                Some("manual") => false,
                Some(_) => {
                    eprintln!("usage: /game start [auto|manual]");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game start [auto|manual]") {
                return None;
            }
            GameApi::start(character, auto).await
        }
        "place" => {
            let coord = match parse_coord(parts.next()) {
                Ok(coord) => coord,
                Err(message) => {
                    eprintln!("{message}");
                    return None;
                }
            };
            let length = match parts.next().and_then(|value| value.parse::<u8>().ok()) {
                Some(length @ 1..=4) => length,
                _ => {
                    eprintln!("длина корабля должна быть 1..4");
                    return None;
                }
            };
            let orientation = match Orientation::parse(parts.next().unwrap_or("H")) {
                Some(orientation) => orientation,
                None => {
                    eprintln!("ориентация должна быть H или V");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game place COORD LENGTH [H|V]") {
                return None;
            }
            GameApi::place_ship(character, coord, length, orientation).await
        }
        "random" => {
            if !no_extra_args(&mut parts, "/game random") {
                return None;
            }
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos() as u64)
                .unwrap_or(1);
            GameApi::place_fleet_randomly(character, seed).await
        }
        "fire" => {
            let coord = match parse_coord(parts.next()) {
                Ok(coord) => coord,
                Err(message) => {
                    eprintln!("{message}");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game fire COORD") {
                return None;
            }
            GameApi::fire(character, coord).await
        }
        "turn" => {
            if !no_extra_args(&mut parts, "/game turn") {
                return None;
            }
            GameApi::request_mita_turn(character).await
        }
        "auto" => {
            let enabled = match parts.next() {
                Some("on") => true,
                Some("off") => false,
                _ => {
                    eprintln!("usage: /game auto on|off");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game auto on|off") {
                return None;
            }
            GameApi::set_auto_reactions(character, enabled).await
        }
        "close" => {
            if !no_extra_args(&mut parts, "/game close") {
                return None;
            }
            GameApi::close(character).await
        }
        "view" => {
            if !no_extra_args(&mut parts, "/game view") {
                return None;
            }
            match GameApi::view(character).await {
                Ok(receipt) => {
                    show_game(&receipt);
                    return None;
                }
                Err(error) => {
                    eprintln!("Игра: {error}");
                    return None;
                }
            }
        }
        other => {
            eprintln!(
                "Неизвестная команда '{other}'. Используй start/place/random/fire/turn/auto/close/view."
            );
            return None;
        }
    };
    match result {
        Ok(outcome) => {
            show_game(&outcome.receipt);
            match outcome.reaction {
                None => None,
                Some(Ok(handle)) => Some(handle.id()),
                Some(Err(error)) => {
                    eprintln!("Реакция Миты не принята: {error}");
                    None
                }
            }
        }
        Err(error) => {
            eprintln!("Игра: {error}");
            None
        }
    }
}
