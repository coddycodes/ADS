//! Records every panic as a `panic` event, even while the profiler is off: a panic is rare
//! and cheap to record. Each frame is a `(module, debugId, offset)` triple, Breakpad's own
//! addressing, so the `.sym` files that symbolicate minidumps symbolicate panics too.

use std::{
    sync::{Once, OnceLock},
    time::Duration,
};

use serde_json::{Value, json};

use super::{event::ProfilerEvent, global, redact::redact_path};

const MAX_FRAMES: usize = 64;
/// How long a panicking thread waits for its record to reach the OS.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(250);

/// Installs the hook once per process. It records the panic, then runs the hook it replaced.
/// It never aborts, so `catch_unwind` and `JoinError::is_panic` recovery keep working. The
/// first call reads the executable's debug ID, so call it off the async workers.
pub fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = main_module();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            record(info);
            previous(info);
        }));
    });
}

/// Sends straight to the store, not through `emit`, which drops events while the profiler is
/// off. Before a runtime starts, or while it is degraded, there is nowhere to record it.
fn record(info: &std::panic::PanicHookInfo<'_>) {
    let Some(handle) = global() else {
        return;
    };
    let mut frames = Vec::with_capacity(MAX_FRAMES);
    let mut walked = 0;
    backtrace::trace(|frame| {
        walked += 1;
        frames.extend(module_offset(frame));
        walked < MAX_FRAMES
    });
    let message = redact_path(info.payload_as_str().unwrap_or("Box<dyn Any>"));
    let thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    // A dependency's location is a path under the builder's home directory.
    let location = info
        .location()
        .map(|location| redact_path(&format!("{}:{}", location.file(), location.line())));
    handle.sender.send(ProfilerEvent {
        kind: "panic",
        source: "server".into(),
        payload: json!({
            "thread": thread,
            "message": message,
            "location": location,
            "frames": frames,
        }),
    });
    // A panic that cannot unwind aborts right after the hook, before the writer's next timed
    // flush. Panics are rare, so the wait costs nothing in practice.
    handle.sender.flush_within(FLUSH_TIMEOUT);
}

/// The executable that holds this code: its load address, its name as symbol files name it,
/// and its Breakpad debug ID.
struct MainModule {
    base: usize,
    name: String,
    debug_id: Option<String>,
}

fn main_module() -> Option<&'static MainModule> {
    static MAIN: OnceLock<Option<MainModule>> = OnceLock::new();
    MAIN.get_or_init(load_main_module).as_ref()
}

fn load_main_module() -> Option<MainModule> {
    let exe = std::env::current_exe().ok()?;
    let mut name = file_name(&exe.to_string_lossy()).to_owned();
    let mut debug_id = None;
    // `ReadCache` reads only the headers and the note or debug directory, not the whole file.
    if let Ok(file) = std::fs::File::open(&exe) {
        let cache = object::ReadCache::new(file);
        if let Ok(parsed) = object::File::parse(&cache) {
            use object::Object;
            if let Ok(Some(build_id)) = parsed.build_id() {
                let mut guid = [0; 16];
                let len = build_id.len().min(16);
                guid[..len].copy_from_slice(&build_id[..len]);
                debug_id = Some(format!("{}0", breakpad_guid(guid)));
            } else if let Ok(Some(uuid)) = parsed.mach_uuid() {
                debug_id = Some(format!("{}0", upper_hex(&uuid)));
            } else if let Ok(Some(pdb)) = parsed.pdb_info() {
                // Windows symbol files are named after the PDB.
                name = file_name(&String::from_utf8_lossy(pdb.path())).to_owned();
                debug_id = Some(format!("{}{:x}", breakpad_guid(pdb.guid()), pdb.age()));
            }
        }
    }
    Some(MainModule {
        base: main_base()?,
        name,
        debug_id,
    })
}

/// Breakpad reads an identifier as a GUID whose first three fields are little-endian.
fn breakpad_guid(mut guid: [u8; 16]) -> String {
    guid[..4].reverse();
    guid[4..6].reverse();
    guid[6..8].reverse();
    upper_hex(&guid)
}

fn upper_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn frame_json(base: usize, ip: usize, other_module: impl FnOnce() -> String) -> Option<Value> {
    let offset = ip.checked_sub(base)?;
    let (module, debug_id) = match main_module() {
        Some(main) if main.base == base => (main.name.clone(), main.debug_id.clone()),
        _ => (other_module(), None),
    };
    Some(json!({ "module": module, "debugId": debug_id, "offset": offset }))
}

#[cfg(unix)]
fn main_base() -> Option<usize> {
    dladdr(main_module as fn() -> Option<&'static MainModule> as *const libc::c_void)
        .map(|info| info.dli_fbase as usize)
}

#[cfg(unix)]
fn dladdr(address: *const libc::c_void) -> Option<libc::Dl_info> {
    // SAFETY: `Dl_info` is plain data, and `dladdr` only reads the loader's tables to fill it.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let found = unsafe { libc::dladdr(address, &mut info) } != 0;
    (found && !info.dli_fbase.is_null()).then_some(info)
}

#[cfg(unix)]
fn module_offset(frame: &backtrace::Frame) -> Option<Value> {
    let info = dladdr(frame.ip())?;
    frame_json(info.dli_fbase as usize, frame.ip() as usize, || {
        if info.dli_fname.is_null() {
            return "unknown".to_owned();
        }
        // SAFETY: a non-null `dli_fname` is a NUL-terminated string owned by the loader.
        let path = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy();
        file_name(&path).to_owned()
    })
}

#[cfg(windows)]
fn main_base() -> Option<usize> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    // SAFETY: a null name asks for the executable's own module handle.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) };
    (!base.is_null()).then_some(base as usize)
}

#[cfg(windows)]
fn module_offset(frame: &backtrace::Frame) -> Option<Value> {
    let base = frame.module_base_address()?;
    frame_json(base as usize, frame.ip() as usize, || {
        use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
        let mut path = [0u16; 1024];
        // SAFETY: `base` is a loaded module's handle and the buffer's length is passed.
        let len = unsafe { GetModuleFileNameW(base, path.as_mut_ptr(), path.len() as u32) };
        let path = String::from_utf16_lossy(&path[..(len as usize).min(path.len())]);
        file_name(&path).to_owned()
    })
}

#[cfg(not(any(unix, windows)))]
fn main_base() -> Option<usize> {
    None
}

#[cfg(not(any(unix, windows)))]
fn module_offset(_frame: &backtrace::Frame) -> Option<Value> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_ids_use_breakpad_byte_order() {
        let build_id: [u8; 16] = std::array::from_fn(|i| i as u8);
        assert_eq!(breakpad_guid(build_id), "030201000504070608090A0B0C0D0E0F");
    }

    #[test]
    fn the_main_module_is_this_executable_with_its_debug_id() {
        let main = main_module().expect("main module");
        let exe = std::env::current_exe().unwrap();
        let exe = exe.file_name().unwrap().to_string_lossy();
        // Windows names a module after its PDB, as its symbol files are.
        assert!(
            main.name == exe || (cfg!(windows) && main.name.ends_with(".pdb")),
            "{}",
            main.name
        );
        if cfg!(target_os = "linux") {
            let id = main.debug_id.as_deref().expect("a GNU build-id");
            assert_eq!(id.len(), 33);
            assert!(id.ends_with('0'));
        }
    }
}
