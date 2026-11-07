use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

use neuromita::{
    character::CharacterMemorySnapshot,
    memory::{MemoryCommand, MemoryId, MemoryPriority},
    presentation::MemoryListPage,
};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{
    runtime::Command,
    view::{AppWindow, MemoryItem, MemoryUi},
};

#[derive(Default)]
pub struct MemoryEditor {
    current: Option<CharacterMemorySnapshot>,
    draft: Option<CharacterMemorySnapshot>,
    draft_id: Option<uuid::Uuid>,
    offset: usize,
    search: String,
}

pub type EditorState = Arc<Mutex<MemoryEditor>>;

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>, state: &EditorState) {
    let ui = window.global::<MemoryUi>();
    let send = dispatch.clone();
    let selected = state.clone();
    let weak = window.as_weak();
    ui.on_select(move |id| {
        let mut editor = selected.lock().expect("memory editor mutex");
        editor.current = None;
        editor.draft = None;
        editor.draft_id = None;
        editor.offset = 0;
        editor.search.clear();
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<MemoryUi>();
            ui.set_character(id);
            ui.set_editing(false);
            ui.set_edit_id("".into());
            ui.set_content("".into());
            ui.set_error("".into());
            ui.set_delete_confirm(false);
            ui.set_search("".into());
            ui.set_offset(0);
            ui.set_next(-1);
            ui.set_active("0".into());
            ui.set_forgotten("0".into());
            ui.set_deleted("0".into());
            ui.set_total("0".into());
            ui.set_items(ModelRc::new(VecModel::from(Vec::<MemoryItem>::new())));
        }
        let Some(character) = neuromita::character::CharacterId::from_index(id) else {
            return;
        };
        send(Command::MemorySelect(character));
    });
    let editor = state.clone();
    let weak = window.as_weak();
    ui.on_page(move |offset, search| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let Ok(offset) = usize::try_from(offset) else {
            return;
        };
        let snapshot = {
            let mut state = editor.lock().expect("memory editor mutex");
            state.offset = offset;
            state.search = search.into();
            state.current.clone()
        };
        if let Some(snapshot) = snapshot {
            match MemoryListPage::from_snapshot(snapshot, Default::default()) {
                Ok(page) => render(&component, &editor, &page),
                Err(error) => component
                    .global::<MemoryUi>()
                    .set_error(error.to_string().into()),
            }
        }
    });
    let editor = state.clone();
    let weak = window.as_weak();
    ui.on_edit(move |id| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<MemoryUi>();
        let mut editor = editor.lock().expect("memory editor mutex");
        let Some(snapshot) = editor.current.clone() else {
            ui.set_error("Memory snapshot is unavailable".into());
            return;
        };
        if id.is_empty() {
            ui.set_edit_id("".into());
            ui.set_content("".into());
        } else {
            let Some(record) = snapshot
                .records()
                .iter()
                .find(|record| record.id.0.to_string() == id.as_str())
            else {
                ui.set_error("Memory is no longer available".into());
                return;
            };
            ui.set_edit_id(id);
            ui.set_content(record.content.as_str().into());
        }
        editor.draft = Some(snapshot);
        editor.draft_id = Some(uuid::Uuid::new_v4());
        ui.set_editing(true);
        ui.set_error("".into());
    });
    let editor = state.clone();
    let weak = window.as_weak();
    ui.on_apply(move |action| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<MemoryUi>();
        if !ui.get_editing() {
            return;
        }
        let editor = editor.lock().expect("memory editor mutex");
        let Some(edit_id) = editor.draft_id else {
            return;
        };
        let Some(expected) = editor.draft.clone() else {
            ui.set_error("Select a memory or choose Add first".into());
            return;
        };
        let result = (|| -> Result<MemoryCommand, String> {
            let value = ui.get_edit_id();
            if action == 0 && value.is_empty() {
                return Ok(MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: ui.get_content().into(),
                });
            }
            let id = MemoryId(uuid::Uuid::parse_str(&value).map_err(|e| e.to_string())?);
            let record = expected
                .records()
                .iter()
                .find(|r| r.id == id)
                .ok_or("Memory is absent from the selected snapshot")?;
            Ok(match action {
                0 => MemoryCommand::Update {
                    id,
                    priority: None,
                    content: ui.get_content().into(),
                },
                1 => MemoryCommand::Forget { id },
                2 => MemoryCommand::SoftDelete { id },
                3 => MemoryCommand::Restore { id },
                4 => MemoryCommand::SetProtection {
                    id,
                    protected: !record.protected,
                },
                _ => return Err("Unknown memory operation".into()),
            })
        })();
        match result {
            Ok(command) => {
                ui.set_editing(false);
                dispatch(Command::MemoryEdit(edit_id, expected, vec![command]));
            }
            Err(error) => ui.set_error(error.into()),
        }
    });
}

pub fn render(window: &AppWindow, state: &EditorState, page: &MemoryListPage) {
    let (offset, search) = {
        let mut state = state.lock().expect("memory editor mutex");
        state.current = Some(page.snapshot().clone());
        (state.offset, state.search.clone())
    };
    let query = neuromita::presentation::MemoryListQuery {
        offset,
        text: Some(search),
        ..Default::default()
    };
    let page = match MemoryListPage::from_snapshot(page.snapshot().clone(), query) {
        Ok(page) => page,
        Err(error) => {
            window
                .global::<MemoryUi>()
                .set_error(error.to_string().into());
            return;
        }
    };
    let ui = window.global::<MemoryUi>();
    ui.set_character(page.snapshot().character().index() as i32);
    ui.set_offset(i32::try_from(offset).unwrap_or(i32::MAX));
    ui.set_next(
        page.next_offset()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(-1),
    );
    let summary = page.summary();
    ui.set_active(summary.active.to_string().into());
    ui.set_forgotten(summary.forgotten.to_string().into());
    ui.set_deleted(summary.deleted.to_string().into());
    ui.set_total(page.total_matches().to_string().into());
    ui.set_items(ModelRc::new(VecModel::from(
        page.records()
            .iter()
            .map(|record| MemoryItem {
                id: record.id.0.to_string().into(),
                text: record.content.as_str().into(),
                state: match record.state {
                    neuromita::memory::MemoryState::Active => 0,
                    neuromita::memory::MemoryState::Forgotten => 1,
                    neuromita::memory::MemoryState::Deleted => 2,
                },
                priority: match record.priority {
                    MemoryPriority::Low => 0,
                    MemoryPriority::Normal => 1,
                    MemoryPriority::High => 2,
                    MemoryPriority::Critical => 3,
                },
                protected: record.protected,
            })
            .collect::<Vec<_>>(),
    )));
}

pub fn saved(window: &AppWindow, state: &EditorState, edit_id: uuid::Uuid) {
    let mut editor = state.lock().expect("memory editor mutex");
    if editor.draft_id != Some(edit_id) {
        return;
    }
    editor.draft = None;
    editor.draft_id = None;
    let ui = window.global::<MemoryUi>();
    ui.set_error("".into());
    ui.set_edit_id("".into());
    ui.set_content("".into());
    ui.set_editing(false);
}

pub fn failed(window: &AppWindow, state: &EditorState, edit_id: uuid::Uuid, error: String) {
    if state.lock().expect("memory editor mutex").draft_id != Some(edit_id) {
        return;
    }
    let ui = window.global::<MemoryUi>();
    ui.set_editing(true);
    ui.set_error(error.into());
}
