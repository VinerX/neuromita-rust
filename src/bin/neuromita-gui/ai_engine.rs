use super::{
    runtime::Command,
    view::{AiEngineUi, AiHardwareDevice, AppWindow},
};
use neuromita::presentation::hardware::HardwareView;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;

pub fn bind(window: &AppWindow, send: Rc<dyn Fn(Command)>) {
    let weak = window.as_weak();
    window.global::<AiEngineUi>().on_refresh_hardware(move || {
        if let Some(window) = weak.upgrade() {
            window.global::<AiEngineUi>().set_detecting(true);
        }
        send(Command::RefreshHardware);
    });
}

pub fn render(window: &AppWindow, snapshot: HardwareView) {
    let ui = window.global::<AiEngineUi>();
    ui.set_devices(ModelRc::new(VecModel::from(
        snapshot
            .devices
            .into_iter()
            .map(|device| AiHardwareDevice {
                name: device.name.into(),
                vendor: device.vendor.into(),
                cuda: device.cuda.into(),
                directml: "".into(),
                capability: device.capability.into(),
                vram: device.memory.into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_hardware_error(snapshot.errors.join("\n").into());
    ui.set_detecting(false);
}
