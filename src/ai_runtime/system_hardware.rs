//! OS inventory, independent of Python environments and inference readiness.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemAdapter {
    pub name: String,
    pub vendor_id: u32,
    pub dedicated_memory_bytes: u64,
    pub luid: [u8; 8],
    pub cuda: Option<CudaDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CudaDevice {
    pub ordinal: i32,
    pub major: i32,
    pub minor: i32,
}

#[derive(Debug, Clone, Default)]
pub struct SystemHardware {
    pub adapters: Vec<SystemAdapter>,
    pub errors: Vec<String>,
}

impl SystemHardware {
    pub fn detect() -> Self {
        #[cfg(windows)]
        return windows::detect();
        #[cfg(not(windows))]
        Self {
            adapters: Vec::new(),
            errors: vec!["System GPU inventory is not implemented on this platform".into()],
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    struct Guid {
        a: u32,
        b: u16,
        c: u16,
        d: [u8; 8],
    }
    #[repr(C)]
    struct Desc {
        description: [u16; 128],
        vendor: u32,
        device: u32,
        subsystem: u32,
        revision: u32,
        video: usize,
        system: usize,
        shared: usize,
        luid: [u8; 8],
        flags: u32,
    }
    #[link(name = "dxgi")]
    unsafe extern "system" {
        fn CreateDXGIFactory1(id: *const Guid, result: *mut *mut c_void) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }
    struct Com(*mut c_void);
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                let table = *(self.0 as *const *const *const c_void);
                let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(*table.add(2));
                release(self.0);
            }
        }
    }
    struct Library(*mut c_void);
    impl Drop for Library {
        fn drop(&mut self) {
            unsafe {
                FreeLibrary(self.0);
            }
        }
    }

    pub fn detect() -> SystemHardware {
        let mut result = SystemHardware::default();
        unsafe {
            let iid = Guid {
                a: 0x770aae78,
                b: 0xf26f,
                c: 0x4dba,
                d: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
            };
            let mut factory = std::ptr::null_mut();
            let status = CreateDXGIFactory1(&iid, &mut factory);
            if status < 0 || factory.is_null() {
                result
                    .errors
                    .push(format!("DXGI factory failed: {status:#x}"));
                return result;
            }
            let factory = Com(factory);
            let table = *(factory.0 as *const *const *const c_void);
            let enumerate: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
                std::mem::transmute(*table.add(12));
            for index in 0..128 {
                let mut adapter = std::ptr::null_mut();
                let status = enumerate(factory.0, index, &mut adapter);
                if status as u32 == 0x887a0002 {
                    break;
                }
                if status < 0 || adapter.is_null() {
                    result
                        .errors
                        .push(format!("DXGI enumerate failed: {status:#x}"));
                    break;
                }
                let adapter = Com(adapter);
                let table = *(adapter.0 as *const *const *const c_void);
                let describe: unsafe extern "system" fn(*mut c_void, *mut Desc) -> i32 =
                    std::mem::transmute(*table.add(10));
                let mut desc: Desc = std::mem::zeroed();
                if describe(adapter.0, &mut desc) < 0 {
                    result.errors.push("DXGI description failed".into());
                    continue;
                }
                if desc.flags & 2 != 0 {
                    continue;
                }
                let length = desc.description.iter().position(|c| *c == 0).unwrap_or(128);
                result.adapters.push(SystemAdapter {
                    name: String::from_utf16_lossy(&desc.description[..length]),
                    vendor_id: desc.vendor,
                    dedicated_memory_bytes: desc.video as u64,
                    luid: desc.luid,
                    cuda: None,
                });
            }
            attach_cuda(&mut result);
        }
        result
    }

    unsafe fn attach_cuda(result: &mut SystemHardware) {
        unsafe {
            let name: Vec<u16> = "nvcuda.dll\0".encode_utf16().collect();
            let module = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), 0x800);
            if module.is_null() {
                if result.adapters.iter().any(|a| a.vendor_id == 0x10de) {
                    result
                        .errors
                        .push("NVIDIA CUDA driver is unavailable".into());
                }
                return;
            }
            let module = Library(module);
            let symbols = [
                c"cuInit",
                c"cuDeviceGetCount",
                c"cuDeviceGet",
                c"cuDeviceGetAttribute",
                c"cuDeviceGetLuid",
            ]
            .map(|name| GetProcAddress(module.0, name.as_ptr()));
            if symbols.iter().any(|p| p.is_null()) {
                result
                    .errors
                    .push("CUDA driver inventory symbols are unavailable".into());
                return;
            }
            let init: unsafe extern "system" fn(u32) -> i32 = std::mem::transmute(symbols[0]);
            let count: unsafe extern "system" fn(*mut i32) -> i32 = std::mem::transmute(symbols[1]);
            let get: unsafe extern "system" fn(*mut i32, i32) -> i32 =
                std::mem::transmute(symbols[2]);
            let attr: unsafe extern "system" fn(*mut i32, i32, i32) -> i32 =
                std::mem::transmute(symbols[3]);
            let luid: unsafe extern "system" fn(*mut c_char, *mut u32, i32) -> i32 =
                std::mem::transmute(symbols[4]);
            let mut size = 0;
            if init(0) != 0 || count(&mut size) != 0 {
                result
                    .errors
                    .push("CUDA driver initialization failed".into());
                return;
            }
            for ordinal in 0..size.min(128) {
                let (mut device, mut major, mut minor, mut mask) = (0, 0, 0, 0);
                let mut identity = [0u8; 8];
                if get(&mut device, ordinal) != 0
                    || attr(&mut major, 75, device) != 0
                    || attr(&mut minor, 76, device) != 0
                    || luid(identity.as_mut_ptr().cast(), &mut mask, device) != 0
                {
                    result
                        .errors
                        .push(format!("CUDA inventory failed for ordinal {ordinal}"));
                    continue;
                }
                if let Some(adapter) = result
                    .adapters
                    .iter_mut()
                    .find(|a| a.vendor_id == 0x10de && a.luid == identity)
                {
                    adapter.cuda = Some(CudaDevice {
                        ordinal,
                        major,
                        minor,
                    });
                }
            }
        }
    }
}
