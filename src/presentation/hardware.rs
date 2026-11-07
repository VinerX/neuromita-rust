use crate::ai_runtime::system_hardware::SystemHardware;

pub struct HardwareDeviceView {
    pub name: String,
    pub vendor: &'static str,
    pub cuda: String,
    pub capability: String,
    pub memory: String,
}

pub struct HardwareView {
    pub devices: Vec<HardwareDeviceView>,
    pub errors: Vec<String>,
}

pub struct HardwarePresenter;

impl HardwarePresenter {
    pub fn present(inventory: SystemHardware) -> HardwareView {
        HardwareView {
            devices: inventory
                .adapters
                .into_iter()
                .map(|adapter| HardwareDeviceView {
                    name: adapter.name,
                    vendor: match adapter.vendor_id {
                        0x10de => "NVIDIA",
                        0x1002 => "AMD",
                        0x8086 => "Intel",
                        _ => "Unknown",
                    },
                    cuda: adapter
                        .cuda
                        .as_ref()
                        .map(|cuda| format!("cuda:{}", cuda.ordinal))
                        .unwrap_or_default(),
                    capability: adapter
                        .cuda
                        .map(|cuda| format!("SM {}.{}", cuda.major, cuda.minor))
                        .unwrap_or_default(),
                    memory: format!(
                        "{:.1} GiB",
                        adapter.dedicated_memory_bytes as f64 / 1073741824.0
                    ),
                })
                .collect(),
            errors: inventory.errors,
        }
    }
}
