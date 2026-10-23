pub mod logging;
mod init;
mod log;

use std::{
    arch::naked_asm,
    cell::Cell,
    ffi::{c_void, CStr},
    panic::PanicHookInfo,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use std::ffi::CString;

use mod_loader::Config;
use windows::{
    Win32::{
        Foundation::{FALSE, HINSTANCE, HMODULE, MAX_PATH, TRUE},
        System::{
            Console::{AllocConsole, GetConsoleWindow},
            LibraryLoader::{GetProcAddress, LoadLibraryA},
            SystemInformation::GetSystemDirectoryA,
            SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH},
        },
        UI::WindowsAndMessaging::{MB_OK, MessageBoxA, SW_SHOW, ShowWindow},
    },
    core::{BOOL, GUID, HRESULT, PCSTR},
};

static NAMES: [&CStr; 6] = [
    c"DirectInput8Create",
    c"DllCanUnloadNow",
    c"DllGetClassObject",
    c"DllRegisterServer",
    c"DllUnregisterServer",
    c"GetdfDIJoystick",
];

static mut M_PROCS: [*const c_void; NAMES.len()] = [ptr::null(); NAMES.len()];
static OWN_MODULE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static ORIGINAL_CREATE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CHAIN_CREATE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

thread_local! {
    static IN_CHAIN_CREATE: Cell<bool> = const { Cell::new(false) };
}

type CreateDirectInput = unsafe extern "system" fn(
    HINSTANCE,
    u32,
    *const GUID,
    *mut *mut c_void,
    *mut c_void,
) -> HRESULT;

/// Selects the first DirectInput provider from the loader's ordered DLL plan.
/// The caller owns the module and must keep it loaded until process exit.
/// This function does not load files and must be called outside DllMain.
///
/// # Safety
/// `module` must be a valid, loaded module handle whose DirectInput8Create
/// export follows the Windows ABI and remains available until process exit.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn ShroudforgeSetDinput8Provider(module: HMODULE) -> BOOL {
    if module.is_invalid() || module.0 == OWN_MODULE.load(Ordering::Acquire) {
        return FALSE;
    }
    let Some(create) =
        (unsafe { GetProcAddress(module, PCSTR(c"DirectInput8Create".as_ptr().cast())) })
    else {
        return FALSE;
    };
    let create = create as *mut c_void;
    if create == DirectInput8Create as *mut c_void
        || create == ORIGINAL_CREATE.load(Ordering::Acquire)
    {
        return FALSE;
    }

    match CHAIN_CREATE.compare_exchange(
        ptr::null_mut(),
        create,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => TRUE,
        Err(selected) if selected == create => TRUE,
        Err(_) => FALSE,
    }
}

#[unsafe(no_mangle)]
unsafe extern "system" fn DirectInput8Create(
    instance: HINSTANCE,
    version: u32,
    interface: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    let original = ORIGINAL_CREATE.load(Ordering::Acquire);
    if original.is_null() {
        return HRESULT(0x80004005u32 as i32);
    }

    IN_CHAIN_CREATE.with(|active| {
        let selected = CHAIN_CREATE.load(Ordering::Acquire);
        if selected.is_null() || active.get() {
            let create: CreateDirectInput = unsafe { std::mem::transmute(original) };
            return unsafe { create(instance, version, interface, output, outer) };
        }

        // A provider may forward back to this proxy. Its nested call must use
        // System32 instead of entering the selected provider a second time.
        active.set(true);
        struct ResetChainCall<'a>(&'a Cell<bool>);
        impl Drop for ResetChainCall<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _reset = ResetChainCall(active);
        let create: CreateDirectInput = unsafe { std::mem::transmute(selected) };
        unsafe { create(instance, version, interface, output, outer) }
    })
}

macro_rules! forward {
    ($name:ident, $idx:expr) => {
        #[unsafe(no_mangle)]
        #[unsafe(naked)]
        unsafe extern "system" fn $name() {
            naked_asm!(
                "jmp [rip + {base} + {offset}]",
                base = sym M_PROCS,
                offset = const $idx * 8,
            );
        }
    };
}

forward!(DllCanUnloadNow,    1);
forward!(DllGetClassObject,  2);
forward!(DllRegisterServer,  3);
forward!(DllUnregisterServer,4);
forward!(GetdfDIJoystick,    5);

#[unsafe(no_mangle)]
extern "system" fn DllMain(
    _hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut c_void,
) -> BOOL {
    #[allow(non_snake_case)] // false positive
    match reason {
        DLL_PROCESS_ATTACH => {
            OWN_MODULE.store(_hinst.0, Ordering::Release);
            std::panic::set_hook(Box::new(panic_handler));

            match unsafe { init_procs() } {
                Ok(()) => {
                    // EML versus ShroudForge: the bridge selects the owner once. EML
                    // runs the original initialization below. ShroudForge loads its
                    // own bootstrap and must not also start EML in this proxy.
                    match mod_loader_bridge::try_start() {
                        mod_loader_bridge::Handoff::Started => {}
                        mod_loader_bridge::Handoff::EmlSelected => {
                            let enable_console = std::env::var("EML_CONSOLE")
                                .map(|s| matches!(s.to_lowercase().as_str(), "true" | "1" | "yes"))
                                .unwrap_or(false);

                            let config = Config::load("eml.json")
                                .unwrap_or_default();

                            if enable_console || config.enable_console {
                                crate::enable_console();
                            }

                            init::init(config);
                        }
                        // A selected ShroudForge installation must not fall through to EML.
                        mod_loader_bridge::Handoff::Failed => return FALSE,
                    }
                }
                Err(e) => {
                    panic!("Failed to initialize dinput8 proxy: {e}");
                }
            }
        }
        DLL_PROCESS_DETACH => {
            // A non-null lpReserved means the process is terminating. Other threads
            // are already gone and thread-local storage (used by logging) may
            // already be destroyed, so do not run any cleanup in that case.
            // EML teardown runs only after EML startup. ShroudForge owns its
            // lifecycle and skipped this proxy's EML initialization.
            if _reserved.is_null()
                && mod_loader_bridge::selected_backend() == mod_loader_bridge::Backend::Eml
            {
                init::deinit();
            }
        }
        _ => {}
    }

    TRUE
}

pub fn enable_console()  {
    unsafe {
        match AllocConsole() {
            Ok(_) => {
                let _ = ShowWindow(GetConsoleWindow(), SW_SHOW);
            }
            Err(e) => {
                println!("Warning: Could not allocate console: {e}");
            }
        }
    }
}

fn panic_handler(info: &PanicHookInfo) {
    let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        format!("{:?}", info.payload())
    };
    let loc = info.location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_default();

    let full = CString::new(format!("Panic at {loc}:\n{msg}"))
        .unwrap_or_else(|_| CString::new("Panic at unknown location").unwrap());

    unsafe {
        MessageBoxA(
            None,
            PCSTR::from_raw(full.as_ptr() as _),
            PCSTR::from_raw(c"Fatal error".as_ptr() as _),
            MB_OK,
        );
    }
}

unsafe fn init_procs() -> Result<(), String> {
    let dll = unsafe {
        let mut buf = [0u8; MAX_PATH as _];
        let len = GetSystemDirectoryA(Some(&mut buf));

        if len == 0 || len >= MAX_PATH {
            return Err("Failed to get system directory".into());
        }

        const NAME: &[u8] = c"\\dinput8.dll".to_bytes_with_nul();

        buf[len as usize..len as usize + NAME.len()]
            .copy_from_slice(NAME);

        match LoadLibraryA(PCSTR::from_raw(buf.as_ptr() as _)) {
            Ok(h) => h,
            Err(e) => {
                let str = CStr::from_bytes_until_nul(&buf)
                    .unwrap_or(c"dinput8.dll from system directory");

                return Err(format!("Could not load {str:?}: {e}"));
            }
        }
    };

    if dll.is_invalid() {
        return Err("Could not load dinput8.dll".into());
    }

    for i in 0..NAMES.len() {
        let addr = unsafe {
            GetProcAddress(dll, PCSTR::from_raw(NAMES[i].as_ptr() as _))
        };

        match addr {
            Some(addr) => unsafe {
                M_PROCS[i] = addr as _;
                if i == 0 {
                    ORIGINAL_CREATE.store(addr as *mut c_void, Ordering::Release);
                }
            },
            None => {
                println!("Warning: {} is missing in dinput8.dll", NAMES[i].to_string_lossy());

                unsafe {
                    M_PROCS[i] = missing_proc as _;
                }
            }
        };
    }

    Ok(())
}

fn missing_proc() {
    panic!("called a missing function on dinput8.dll");
}
