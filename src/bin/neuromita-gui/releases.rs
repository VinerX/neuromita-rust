use std::rc::Rc;

use neuromita::presentation::{ReleaseLoadState, ReleasesSnapshot};
use slint::{ModelRc, VecModel};

use super::{
    runtime::Command,
    view::{AppWindow, ReleaseEntry, ReleaseItem},
};

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>) {
    window.on_releases_refresh(move || dispatch(Command::RefreshReleases));
}

pub fn render(window: &AppWindow, snapshot: &ReleasesSnapshot) {
    window.set_releases(ModelRc::new(VecModel::from(
        snapshot
            .records
            .iter()
            .take(3)
            .map(|record| ReleaseItem {
                title: record.title.as_str().into(),
                date: record.date.as_str().into(),
            })
            .collect::<Vec<_>>(),
    )));

    window.set_release_loading(
        !snapshot.closed && matches!(snapshot.state, ReleaseLoadState::Loading),
    );
    window.set_release_error(match &snapshot.state {
        ReleaseLoadState::Failed(error) => error.to_string().into(),
        _ => "".into(),
    });
    let (mut stable, mut preview) = (Vec::new(), Vec::new());
    for record in snapshot.records.iter() {
        let entry = ReleaseEntry {
            id: record.id.as_str().into(),
            title: record.title.as_str().into(),
            date: record.date.as_str().into(),
            summary: record.summary.as_str().into(),
            details: record.details.as_str().into(),
            url: record.url.as_str().into(),
            prerelease: record.prerelease,
        };
        if record.prerelease {
            preview.push(entry);
        } else {
            stable.push(entry);
        }
    }
    window.set_release_releases(ModelRc::new(VecModel::from(stable)));
    window.set_release_prereleases(ModelRc::new(VecModel::from(preview)));
}
