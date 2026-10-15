//! CoreSimulator and SimulatorKit loaded into the `bibcode` process, and a booted
//! simulator's screen read through them. Every private name comes from spec §0 "Results".
//!
//! The IO ports, display descriptors and screen properties are `ROCKRemoteProxy` objects:
//! an unknown selector raises an Objective-C exception, and objc2's `msg_send!` cannot
//! check a proxy's forwarded methods. So every send goes through [`guarded`], which calls
//! `objc_msgSend` with the exact signature §0 recorded and catches any exception before it
//! reaches a Rust frame, and every send to a proxy is first gated by `respondsToSelector:` or
//! `conformsToProtocol:`. The guard (`guard::probe_with`) has already checked that each
//! class, protocol and selector exists with that signature.

use std::{
    ffi::{CStr, CString, c_void},
    os::unix::ffi::OsStrExt as _,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    ptr::{NonNull, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use block2::{Block, ManualBlockEncoding, RcBlock};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::{
    exception,
    ffi::{self, objc_method_description},
    rc::{Retained, autoreleasepool},
    runtime::{AnyClass, AnyObject, AnyProtocol, Bool, Sel},
};
use objc2_core_foundation::{CFRetained, CFType};
use objc2_foundation::{NSError, NSString, NSUUID};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use tokio::sync::watch;

use super::{
    guard::{GuardResult, Owner, SymbolResolver, probe_with},
    lock,
    pixels::{Bgra, IosOrientation, upright_rgb},
};
use crate::device::{DeviceError, catalog::AvailabilityKind};

const CORE_SIMULATOR: &str =
    "/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator";
/// `'BGRA'`, the framebuffer's pixel format.
const BGRA: u32 = 0x4247_5241;
/// How long `attach` waits for the first framebuffer after registering.
const FIRST_SURFACE_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_SURFACE_POLL: Duration = Duration::from_millis(50);

/// SimulatorKit ships inside Xcode: `…/Xcode.app/Contents/SharedFrameworks`, next to the
/// developer directory `…/Xcode.app/Contents/Developer`.
fn simulator_kit_path(developer_dir: &Path) -> PathBuf {
    developer_dir
        .parent()
        .unwrap_or(developer_dir)
        .join("SharedFrameworks/SimulatorKit.framework/SimulatorKit")
}

/// A `dlopen` handle. It is never closed: Objective-C classes cannot be unloaded.
#[derive(Clone, Copy)]
struct Library(NonNull<c_void>);

// SAFETY: a `dlopen` handle is an opaque token that `dlsym` accepts from any thread.
unsafe impl Send for Library {}
// SAFETY: as above; the handle is never mutated or closed.
unsafe impl Sync for Library {}

impl Library {
    fn open(path: &Path) -> Result<Self, String> {
        let name = path.display().to_string();
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| name.clone())?;
        // SAFETY: `c_path` is a valid C string. Loading runs the frameworks' initializers,
        // which spike S1–S3 ran in-process the same way.
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        NonNull::new(handle).map(Self).ok_or(name)
    }

    /// The address of `name` in this library, or anywhere in the process.
    fn symbol(self, name: &str) -> Option<Symbol> {
        let c_name = CString::new(name).ok()?;
        // SAFETY: the handle came from `dlopen` and `c_name` is a valid C string.
        let own = unsafe { libc::dlsym(self.0.as_ptr(), c_name.as_ptr()) };
        let found = if own.is_null() {
            // SAFETY: as above; `RTLD_DEFAULT` searches every loaded image (libxpc).
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c_name.as_ptr()) }
        } else {
            own
        };
        NonNull::new(found).map(Symbol)
    }
}

/// A C function's address, for Task 8 to call with its §0 signature.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Symbol(#[allow(dead_code)] NonNull<c_void>); // Task 8 calls it.

// SAFETY: a code address is immutable and valid on every thread.
unsafe impl Send for Symbol {}
// SAFETY: as above.
unsafe impl Sync for Symbol {}

/// The C functions live input calls (spec §0, S1).
#[allow(dead_code)] // Task 8 sends input through them.
pub(crate) struct HidSymbols {
    pub mouse_event: Symbol,
    pub button: Symbol,
    pub hid_arbitrary: Symbol,
    pub endpoint_create_mach_port: Symbol,
    pub connection_from_endpoint: Symbol,
    pub enable_sim2host: Symbol,
}

/// Loaded frameworks and resolved symbols. It exists only once the guard found every name.
pub(crate) struct CoreSim {
    developer_dir: PathBuf,
    #[allow(dead_code)] // Task 8 sends input through them.
    pub hid: HidSymbols,
}

/// The guard's view of the loaded frameworks.
struct DlsymResolver {
    developer_dir: PathBuf,
    simulator_kit: Library,
}

fn c_string(name: &str) -> Option<CString> {
    CString::new(name).ok()
}

fn class_named(name: &str) -> Option<&'static AnyClass> {
    AnyClass::get(&c_string(name)?)
}

/// The type encoding of a method description, if it names one.
fn description_types(description: &objc_method_description) -> Option<String> {
    description.name?;
    if description.types.is_null() {
        return None;
    }
    // SAFETY: the runtime returns a NUL-terminated encoding that lives as long as the image.
    Some(
        unsafe { CStr::from_ptr(description.types) }
            .to_string_lossy()
            .into_owned(),
    )
}

impl SymbolResolver for DlsymResolver {
    fn class(&self, name: &str) -> bool {
        class_named(name).is_some()
    }

    fn protocol(&self, name: &str) -> bool {
        c_string(name).is_some_and(|name| AnyProtocol::get(&name).is_some())
    }

    fn responds(&self, owner: Owner, selector: &str) -> Option<String> {
        let selector = Sel::register(&c_string(selector)?);
        let method = match owner {
            Owner::Instance(class) => class_named(class)?.instance_method(selector)?,
            Owner::Class(class) => class_named(class)?.class_method(selector)?,
            Owner::Protocol(protocol) => {
                let protocol = AnyProtocol::get(&c_string(protocol)?)?;
                return [Bool::YES, Bool::NO].into_iter().find_map(|required| {
                    // SAFETY: `protocol` is a live runtime protocol; the call only reads it.
                    let description = unsafe {
                        ffi::protocol_getMethodDescription(protocol, selector, required, Bool::YES)
                    };
                    description_types(&description)
                });
            }
        };
        // SAFETY: `method` is a live runtime method.
        let types = unsafe { ffi::method_getTypeEncoding(method) };
        if types.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated encoding that lives as long as the image.
        Some(
            unsafe { CStr::from_ptr(types) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn function(&self, name: &str) -> bool {
        self.simulator_kit.symbol(name).is_some()
    }

    fn load(&self) -> Result<Arc<CoreSim>, String> {
        let symbol = |name: &str| {
            self.simulator_kit
                .symbol(name)
                .ok_or_else(|| name.to_owned())
        };
        Ok(Arc::new(CoreSim {
            developer_dir: self.developer_dir.clone(),
            hid: HidSymbols {
                mouse_event: symbol("IndigoHIDMessageForMouseNSEvent")?,
                button: symbol("IndigoHIDMessageForButton")?,
                hid_arbitrary: symbol("IndigoHIDMessageForHIDArbitrary")?,
                endpoint_create_mach_port: symbol("xpc_endpoint_create_mach_port_4sim")?,
                connection_from_endpoint: symbol("xpc_connection_create_from_endpoint")?,
                enable_sim2host: symbol("xpc_connection_enable_sim2host_4sim")?,
            },
        }))
    }
}

/// Loads both frameworks for `developer_dir` and checks every private name; never panics.
pub(crate) fn probe(developer_dir: &Path) -> GuardResult {
    probe_frameworks(Path::new(CORE_SIMULATOR), developer_dir)
}

/// [`probe`] with CoreSimulator at `core_simulator`.
fn probe_frameworks(core_simulator: &Path, developer_dir: &Path) -> GuardResult {
    let opened = Library::open(core_simulator)
        .and_then(|_| Library::open(&simulator_kit_path(developer_dir)));
    match opened {
        Ok(simulator_kit) => probe_with(&DlsymResolver {
            developer_dir: developer_dir.to_owned(),
            simulator_kit,
        }),
        Err(missing) => {
            tracing::warn!(%missing, "could not load a simulator framework; simulators run view-only");
            GuardResult::ViewOnly {
                kind: AvailabilityKind::XcodeUnsupported,
                missing,
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sending messages

type Id = *mut AnyObject;

/// Runs one Objective-C send. An exception it raises is caught here, before it can unwind
/// through a Rust frame, and becomes an error naming the selector.
fn guarded<R>(selector: &CStr, send: impl FnOnce() -> R) -> Result<R, DeviceError> {
    exception::catch(AssertUnwindSafe(send)).map_err(|exception| {
        let selector = selector.to_string_lossy();
        tracing::warn!(%selector, ?exception, "a simulator framework call raised an exception");
        DeviceError::Protocol(format!("{selector} raised an Objective-C exception"))
    })
}

/// `objc_msgSend` as a function of `(receiver, selector, args…) -> R`.
///
/// # Safety
///
/// `F` must be an `unsafe extern "C-unwind" fn(Id, Sel, …) -> R` whose arguments and return
/// type match the receiver's method exactly; none of the §0 methods return a struct.
unsafe fn msg_send_fn<F: Copy>() -> F {
    const { assert!(size_of::<F>() == size_of::<unsafe extern "C-unwind" fn()>()) };
    let send: unsafe extern "C-unwind" fn() = ffi::objc_msgSend;
    // SAFETY: function pointers have one size and representation; the caller picks the
    // signature that matches the method.
    unsafe { std::mem::transmute_copy(&send) }
}

/// Sends `selector` with no arguments.
///
/// # Safety
///
/// `receiver` is a live object (or nil) whose `selector` takes no arguments and returns `R`.
unsafe fn send0<R>(receiver: Id, selector: Sel) -> R {
    // SAFETY: the caller guarantees the signature.
    unsafe { msg_send_fn::<unsafe extern "C-unwind" fn(Id, Sel) -> R>()(receiver, selector) }
}

/// # Safety
///
/// As [`send0`], with one argument of type `A`.
unsafe fn send1<A, R>(receiver: Id, selector: Sel, a: A) -> R {
    // SAFETY: the caller guarantees the signature.
    unsafe { msg_send_fn::<unsafe extern "C-unwind" fn(Id, Sel, A) -> R>()(receiver, selector, a) }
}

/// # Safety
///
/// As [`send0`], with two arguments.
unsafe fn send2<A, B, R>(receiver: Id, selector: Sel, a: A, b: B) -> R {
    // SAFETY: the caller guarantees the signature.
    unsafe {
        msg_send_fn::<unsafe extern "C-unwind" fn(Id, Sel, A, B) -> R>()(receiver, selector, a, b)
    }
}

/// # Safety
///
/// As [`send0`], with five arguments.
unsafe fn send5<A, B, C, D, E>(receiver: Id, selector: Sel, a: A, b: B, c: C, d: D, e: E) {
    // SAFETY: the caller guarantees the signature.
    unsafe {
        msg_send_fn::<unsafe extern "C-unwind" fn(Id, Sel, A, B, C, D, E)>()(
            receiver, selector, a, b, c, d, e,
        );
    }
}

fn sel(name: &CStr) -> Sel {
    Sel::register(name)
}

fn protocol_error(message: impl Into<String>) -> DeviceError {
    DeviceError::Protocol(message.into())
}

/// An object CoreSimulator returned, retained.
struct Object(Retained<AnyObject>);

// SAFETY: CoreSimulator's devices, IO clients and remote proxies are XPC-backed objects that
// Simulator.app and idb message from arbitrary threads, and spike S1–S3 did the same; retain
// and release are atomic.
unsafe impl Send for Object {}
// SAFETY: as for `Send`. A shared `Object` is only `Screen`'s descriptor and UUID, which are
// messaged once more after `attach`, by `detach`, which an atomic flag lets run only once.
unsafe impl Sync for Object {}

impl Object {
    fn ptr(&self) -> Id {
        Retained::as_ptr(&self.0).cast_mut()
    }

    /// Retains an object a getter returned (autoreleased, or owned by its parent).
    fn retain(object: Id) -> Option<Self> {
        // SAFETY: `object` is nil or a live object the callee returned.
        unsafe { Retained::retain(object) }.map(Self)
    }

    /// `[self respondsToSelector:selector]`, false when it raises.
    fn responds(&self, selector: &CStr) -> bool {
        let target = sel(selector);
        guarded(c"respondsToSelector:", || {
            // SAFETY: NSObject's `respondsToSelector:` takes a SEL and returns BOOL; every
            // proxy implements it.
            unsafe { send1::<Sel, Bool>(self.ptr(), sel(c"respondsToSelector:"), target) }
        })
        .is_ok_and(Bool::as_bool)
    }

    /// `[self conformsToProtocol:protocol]`, false when the protocol is unknown or it raises.
    fn conforms(&self, protocol: &CStr) -> bool {
        let Some(protocol) = AnyProtocol::get(protocol) else {
            return false;
        };
        let protocol: *const AnyProtocol = protocol;
        guarded(c"conformsToProtocol:", || {
            // SAFETY: NSObject's `conformsToProtocol:` takes a Protocol and returns BOOL.
            unsafe {
                send1::<*const AnyProtocol, Bool>(self.ptr(), sel(c"conformsToProtocol:"), protocol)
            }
        })
        .is_ok_and(Bool::as_bool)
    }

    /// A getter that returns an object (`@16@0:8`), gated by `respondsToSelector:`.
    fn get(&self, selector: &CStr) -> Result<Self, DeviceError> {
        if !self.responds(selector) {
            return Err(missing(selector));
        }
        // SAFETY: §0 records `selector` as returning an object and taking no arguments.
        let object = guarded(selector, || unsafe {
            send0::<Id>(self.ptr(), sel(selector))
        })?;
        Self::retain(object).ok_or_else(|| nil(selector))
    }

    /// A getter that returns a scalar `R` (`I`, `S` or `Q`), gated by `respondsToSelector:`.
    fn scalar<R>(&self, selector: &CStr) -> Result<R, DeviceError> {
        if !self.responds(selector) {
            return Err(missing(selector));
        }
        // SAFETY: the caller's `R` is the scalar §0 records for `selector`.
        guarded(selector, || unsafe {
            send0::<R>(self.ptr(), sel(selector))
        })
    }

    /// The elements of an `NSArray`.
    fn elements(&self) -> Result<Vec<Self>, DeviceError> {
        // SAFETY: `count` returns NSUInteger.
        let count = guarded(c"count", || unsafe {
            send0::<usize>(self.ptr(), sel(c"count"))
        })?;
        (0..count)
            .map(|index| {
                // SAFETY: `objectAtIndex:` takes NSUInteger, returns an object; `index` is
                // in bounds.
                let object = guarded(c"objectAtIndex:", || unsafe {
                    send1::<usize, Id>(self.ptr(), sel(c"objectAtIndex:"), index)
                })?;
                Self::retain(object).ok_or_else(|| nil(c"objectAtIndex:"))
            })
            .collect()
    }
}

fn missing(selector: &CStr) -> DeviceError {
    let selector = selector.to_string_lossy();
    tracing::warn!(%selector, "a simulator object does not respond to a selector BiBCode uses");
    protocol_error(format!("the simulator does not respond to {selector}"))
}

fn nil(selector: &CStr) -> DeviceError {
    protocol_error(format!("{} returned nil", selector.to_string_lossy()))
}

/// The description of an `NSError` a call filled in, or the selector that failed.
fn error_from(selector: &CStr, error: Id) -> DeviceError {
    // SAFETY: the out-parameter is nil or an autoreleased `NSError`.
    let error = unsafe { Retained::retain(error.cast::<NSError>()) };
    let selector = selector.to_string_lossy();
    match error {
        Some(error) => {
            let description = error.localizedDescription().to_string();
            tracing::warn!(%selector, %description, "a simulator framework call failed");
            protocol_error(format!("{selector} failed: {description}"))
        }
        None => protocol_error(format!("{selector} returned nil")),
    }
}

/// The booted `SimDevice` whose UDID is `udid`.
fn find_device(sim: &CoreSim, udid: &str) -> Result<Object, DeviceError> {
    let class = AnyClass::get(c"SimServiceContext").ok_or_else(|| missing(c"SimServiceContext"))?;
    let class: Id = std::ptr::from_ref(class).cast::<AnyObject>().cast_mut();
    let developer_dir = NSString::from_str(&sim.developer_dir.to_string_lossy());
    let developer_dir: Id = Retained::as_ptr(&developer_dir)
        .cast::<AnyObject>()
        .cast_mut();
    let shared = c"sharedServiceContextForDeveloperDir:error:";
    let mut error: Id = null_mut();
    // SAFETY: `+sharedServiceContextForDeveloperDir:error:` is `@32@0:8@16^@24` (checked by
    // the guard); `developer_dir` is a live NSString and `error` a valid out-pointer.
    let context = guarded(shared, || unsafe {
        send2::<Id, *mut Id, Id>(class, sel(shared), developer_dir, &raw mut error)
    })?;
    let context = Object::retain(context).ok_or_else(|| error_from(shared, error))?;
    let default_set = c"defaultDeviceSetWithError:";
    let mut error: Id = null_mut();
    // SAFETY: `-defaultDeviceSetWithError:` is `@24@0:8^@16` (checked by the guard).
    let set = guarded(default_set, || unsafe {
        send1::<*mut Id, Id>(context.ptr(), sel(default_set), &raw mut error)
    })?;
    let set = Object::retain(set).ok_or_else(|| error_from(default_set, error))?;
    for device in set.get(c"devices")?.elements()? {
        let Ok(uuid) = device.get(c"UDID") else {
            continue;
        };
        // SAFETY: `-UDID` returns an `NSUUID` (§0).
        let uuid = unsafe { Retained::cast_unchecked::<NSUUID>(uuid.0) };
        if uuid.UUIDString().to_string().eq_ignore_ascii_case(udid) {
            return Ok(device);
        }
    }
    Err(protocol_error("CoreSimulator does not list the simulator"))
}

/// Whether a port descriptor is the main display: it conforms to the three display
/// protocols, has a framebuffer, and its state's `displayClass` is 0 (spec §0).
fn is_main_display(descriptor: &Object) -> bool {
    let displays = [
        c"SimScreen",
        c"SimDisplayIOSurfaceRenderable",
        c"SimDisplayRenderable",
    ];
    if !displays
        .iter()
        .all(|protocol| descriptor.conforms(protocol))
        || !descriptor.responds(c"framebufferSurface")
    {
        return false;
    }
    descriptor
        .get(c"state")
        .and_then(|state| state.scalar::<u16>(c"displayClass"))
        .is_ok_and(|class| class == 0)
}

// ---------------------------------------------------------------------------------------------
// The screen

/// A retained framebuffer IOSurface.
#[derive(Clone)]
pub(crate) struct IoSurface(CFRetained<IOSurfaceRef>);

// SAFETY: IOSurface is thread-safe: its handle can be shared across threads and processes,
// and its pixels are only read between `IOSurfaceLock` and `IOSurfaceUnlock`.
unsafe impl Send for IoSurface {}
// SAFETY: as above.
unsafe impl Sync for IoSurface {}

impl IoSurface {
    /// Retains `object` when it is an IOSurface (toll-free bridged with `IOSurfaceRef`).
    fn retain(object: Id) -> Option<Self> {
        let object = NonNull::new(object)?;
        // SAFETY: `object` is a live object, and every Objective-C object is a CFType that
        // `CFGetTypeID` accepts; `downcast` then checks it is an IOSurface.
        let object = unsafe { CFRetained::retain(object.cast::<CFType>()) };
        object.downcast::<IOSurfaceRef>().ok().map(Self)
    }

    /// The surface's portrait size.
    pub(crate) fn size(&self) -> (u32, u32) {
        let width = u32::try_from(self.0.width()).unwrap_or(0);
        let height = u32::try_from(self.0.height()).unwrap_or(0);
        (width, height)
    }

    /// The surface itself, for the encoder (Task 7).
    #[allow(dead_code)] // Task 7 wraps it in a CVPixelBuffer.
    pub(crate) fn as_surface(&self) -> &IOSurfaceRef {
        &self.0
    }

    /// Reads the surface upright for `orientation`, at most `max_width` wide, as RGB.
    fn read_rgb(
        &self,
        orientation: IosOrientation,
        max_width: u32,
    ) -> Result<(Vec<u8>, u32, u32), DeviceError> {
        let surface = &self.0;
        if surface.pixel_format() != BGRA
            || surface.bytes_per_element() != 4
            || surface.plane_count() != 0
        {
            return Err(protocol_error("the simulator framebuffer is not BGRA"));
        }
        let locked = SurfaceLock::read(surface)?;
        let (width, height) = (surface.width(), surface.height());
        let bytes_per_row = surface.bytes_per_row();
        let len = bytes_per_row
            .saturating_mul(height)
            .min(surface.alloc_size());
        // SAFETY: the surface is locked for reading until `locked` drops, and its base
        // address points at `alloc_size` bytes, of which `len` are read.
        let pixels = unsafe {
            std::slice::from_raw_parts(surface.base_address().as_ptr().cast::<u8>(), len)
        };
        let source = Bgra {
            pixels,
            width: u32::try_from(width).map_err(|_| protocol_error("framebuffer too wide"))?,
            height: u32::try_from(height).map_err(|_| protocol_error("framebuffer too tall"))?,
            bytes_per_row,
        };
        let frame = upright_rgb(&source, orientation.turn(), max_width)
            .ok_or_else(|| protocol_error("the simulator framebuffer is smaller than its size"));
        drop(locked);
        frame
    }
}

/// A read lock on a surface, released on drop.
struct SurfaceLock<'a>(&'a IOSurfaceRef);

impl<'a> SurfaceLock<'a> {
    fn read(surface: &'a IOSurfaceRef) -> Result<Self, DeviceError> {
        // SAFETY: a null seed pointer is allowed.
        let status = unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, null_mut()) };
        if status != 0 {
            tracing::warn!(status, "IOSurfaceLock failed");
            return Err(protocol_error("could not lock the simulator framebuffer"));
        }
        Ok(Self(surface))
    }
}

impl Drop for SurfaceLock<'_> {
    fn drop(&mut self) {
        // SAFETY: the surface was locked read-only by `read`; a null seed is allowed.
        let status = unsafe { self.0.unlock(IOSurfaceLockOptions::ReadOnly, null_mut()) };
        if status != 0 {
            tracing::warn!(status, "IOSurfaceUnlock failed");
        }
    }
}

/// The block signatures CoreSimulator needs: it sends the blocks across XPC and refuses a
/// block without one ("Block is missing signature field").
struct FrameBlock;
// SAFETY: `void (^)(void)` is `v8@?0` on 64-bit, matching `Arguments` and `Return`.
unsafe impl ManualBlockEncoding for FrameBlock {
    type Arguments = ();
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v8@?0";
}

struct SurfacesBlock;
// SAFETY: `void (^)(id, id)` is `v24@?0@8@16` on 64-bit, matching `Arguments`.
unsafe impl ManualBlockEncoding for SurfacesBlock {
    type Arguments = (Id, Id);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v24@?0@8@16";
}

struct PropertiesBlock;
// SAFETY: `void (^)(id)` is `v16@?0@8` on 64-bit, matching `Arguments`.
unsafe impl ManualBlockEncoding for PropertiesBlock {
    type Arguments = (Id,);
    type Return = ();
    const ENCODING_CSTR: &'static CStr = c"v16@?0@8";
}

/// What the callbacks update. They run on the screen's serial queue and only replace the
/// surface under a mutex and update watches: nothing there blocks or touches Tokio.
struct ScreenState {
    surface: Mutex<Option<IoSurface>>,
    /// Counts frames; consumers wait for it to change.
    damage: watch::Sender<u64>,
    orientation: watch::Sender<IosOrientation>,
}

// The callbacks run on the dispatch queue's threads, so what they capture must be shareable.
const _: () = {
    const fn shareable<T: Send + Sync>() {}
    shareable::<ScreenState>();
};

impl ScreenState {
    fn tick(&self) {
        self.damage
            .send_modify(|frames| *frames = frames.wrapping_add(1));
    }

    fn set_surface(&self, surface: IoSurface) {
        *lock(&self.surface) = Some(surface);
        self.tick();
    }

    fn set_orientation(&self, orientation: IosOrientation) {
        self.orientation.send_if_modified(|current| {
            let changed = *current != orientation;
            *current = orientation;
            changed
        });
        self.tick();
    }
}

/// `-[SimScreenProperties uiOrientation]` of a properties object; `None` if unreadable.
fn ui_orientation(properties: &Object) -> Option<IosOrientation> {
    let value = properties.scalar::<u32>(c"uiOrientation").ok()?;
    let orientation = IosOrientation::from_ui(value);
    if orientation.is_none() {
        tracing::debug!(value, "the simulator reported an unknown UI orientation");
    }
    orientation
}

/// A booted simulator's main display, with its `SimScreen` callbacks registered on a
/// private serial queue until [`detach`](Self::detach) or drop.
pub(crate) struct Screen {
    descriptor: Object,
    uuid: Object,
    /// Kept alive while the callbacks are registered on it.
    _queue: DispatchRetained<DispatchQueue>,
    state: Arc<ScreenState>,
    detached: AtomicBool,
}

impl Screen {
    /// Finds `udid`'s main display and registers its frame, surfaces-changed and
    /// properties-changed callbacks. Blocking: it messages CoreSimulator and waits up to
    /// five seconds for the first framebuffer, so call it off the async runtime.
    pub(crate) fn attach(sim: &CoreSim, udid: &str) -> Result<Self, DeviceError> {
        let screen = autoreleasepool(|_| Self::register(sim, udid))?;
        let deadline = Instant::now() + FIRST_SURFACE_TIMEOUT;
        while screen.surface().is_none() {
            if Instant::now() >= deadline {
                return Err(protocol_error("the simulator has no framebuffer yet"));
            }
            std::thread::sleep(FIRST_SURFACE_POLL);
        }
        Ok(screen)
    }

    fn register(sim: &CoreSim, udid: &str) -> Result<Self, DeviceError> {
        let device = find_device(sim, udid)?;
        let ports = device.get(c"io")?.get(c"ioPorts")?.elements()?;
        let descriptor = ports
            .iter()
            .filter(|port| port.conforms(c"SimDeviceIOPortInterface"))
            .filter_map(|port| port.get(c"descriptor").ok())
            .find(is_main_display)
            .ok_or_else(|| protocol_error("the simulator has no main display"))?;
        let orientation = descriptor
            .get(c"screenProperties")
            .ok()
            .and_then(|properties| ui_orientation(&properties))
            .unwrap_or(IosOrientation::Portrait);
        let state = Arc::new(ScreenState {
            surface: Mutex::new(None),
            damage: watch::Sender::new(0),
            orientation: watch::Sender::new(orientation),
        });
        if let Ok(surface) = descriptor.get(c"framebufferSurface")
            && let Some(surface) = IoSurface::retain(surface.ptr())
        {
            state.set_surface(surface);
        }
        let queue = DispatchQueue::new("dev.bibcode.ios-screen", DispatchQueueAttr::SERIAL);
        let uuid = NSUUID::new();
        // SAFETY: an `NSUUID` is an object.
        let uuid = Object(unsafe { Retained::cast_unchecked::<AnyObject>(uuid) });
        let frame = {
            let state = state.clone();
            RcBlock::with_encoding::<_, _, _, FrameBlock>(move || state.tick())
        };
        let surfaces = {
            let state = state.clone();
            RcBlock::with_encoding::<_, _, _, SurfacesBlock>(move |framebuffer: Id, _masked: Id| {
                if let Some(surface) = IoSurface::retain(framebuffer) {
                    state.set_surface(surface);
                }
            })
        };
        let properties = {
            let state = state.clone();
            RcBlock::with_encoding::<_, _, _, PropertiesBlock>(move |properties: Id| {
                autoreleasepool(|_| {
                    let orientation = Object::retain(properties)
                        .and_then(|properties| ui_orientation(&properties));
                    if let Some(orientation) = orientation {
                        state.set_orientation(orientation);
                    }
                });
            })
        };
        let register = c"registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:";
        if !descriptor.responds(register) {
            return Err(missing(register));
        }
        let queue_ptr: Id = DispatchRetained::as_ptr(&queue).as_ptr().cast();
        // The blocks stay alive until this returns; the callee copies them to keep.
        let frame: *const Block<dyn Fn()> = &*frame;
        let surfaces: *const Block<dyn Fn(Id, Id)> = &*surfaces;
        let properties: *const Block<dyn Fn(Id)> = &*properties;
        // SAFETY: §0 records `v56@0:8@16@24@?32@?40@?48`: an NSUUID, a dispatch queue and
        // three blocks, which the callee copies. The blocks capture only an `Arc` of state
        // that is `Send + Sync`.
        guarded(register, || unsafe {
            send5(
                descriptor.ptr(),
                sel(register),
                uuid.ptr(),
                queue_ptr,
                frame,
                surfaces,
                properties,
            );
        })?;
        Ok(Self {
            descriptor,
            uuid,
            _queue: queue,
            state,
            detached: AtomicBool::new(false),
        })
    }

    /// Unregisters the callbacks once; later calls do nothing. Callbacks already queued may
    /// still run: they hold their own reference to the state.
    pub(crate) fn detach(&self) {
        if self.detached.swap(true, Ordering::SeqCst) {
            return;
        }
        let unregister = c"unregisterScreenCallbacksWithUUID:";
        if !self.descriptor.responds(unregister) {
            return;
        }
        // SAFETY: §0 records `v24@0:8@16`: one NSUUID, the one registered.
        let _ = autoreleasepool(|_| {
            guarded(unregister, || unsafe {
                send1::<Id, ()>(self.descriptor.ptr(), sel(unregister), self.uuid.ptr());
            })
        });
    }

    /// Changes once per new frame, surface or orientation.
    pub(crate) fn damage(&self) -> watch::Receiver<u64> {
        self.state.damage.subscribe()
    }

    /// The UI orientation the simulator reports.
    pub(crate) fn orientation(&self) -> watch::Receiver<IosOrientation> {
        self.state.orientation.subscribe()
    }

    /// The current framebuffer, retained.
    pub(crate) fn surface(&self) -> Option<IoSurface> {
        lock(&self.state.surface).clone()
    }

    /// The framebuffer's portrait size; (0, 0) before the first one.
    pub(crate) fn size(&self) -> (u32, u32) {
        self.surface().map_or((0, 0), |surface| surface.size())
    }

    /// The current frame upright in the UI orientation, at most `max_width` wide, as RGB,
    /// with that orientation. Copies about 12 MB, so call it off the async runtime.
    pub(crate) fn read_rgb(&self, max_width: u32) -> Result<RgbFrame, DeviceError> {
        let surface = self
            .surface()
            .ok_or_else(|| protocol_error("the simulator has no framebuffer"))?;
        let orientation = *self.state.orientation.borrow();
        let (rgb, width, height) = surface.read_rgb(orientation, max_width)?;
        Ok(RgbFrame {
            rgb,
            width,
            height,
            orientation,
        })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.detach();
    }
}

/// One upright frame.
pub(crate) struct RgbFrame {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub orientation: IosOrientation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulator_kit_lives_next_to_the_developer_dir() {
        assert_eq!(
            simulator_kit_path(Path::new("/Applications/Xcode.app/Contents/Developer")),
            Path::new(
                "/Applications/Xcode.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit"
            )
        );
    }

    #[test]
    fn a_missing_framework_gives_view_only() {
        // Nonexistent paths, so the unit suite never loads the host's frameworks.
        let probed = probe_frameworks(
            Path::new("/nonexistent/CoreSimulator"),
            Path::new("/nonexistent/Xcode.app/Contents/Developer"),
        );
        match probed {
            GuardResult::ViewOnly { kind, missing } => {
                assert_eq!(kind, AvailabilityKind::XcodeUnsupported);
                assert_eq!(missing, "/nonexistent/CoreSimulator");
            }
            GuardResult::Live(_) => panic!("no frameworks there"),
        }
    }

    #[test]
    fn an_objective_c_exception_becomes_an_error() {
        let object = NSString::from_str("not a proxy");
        let object: Id = Retained::as_ptr(&object).cast::<AnyObject>().cast_mut();
        let selector = c"framebufferSurface";
        // SAFETY: an unknown selector makes `doesNotRecognizeSelector:` raise, which
        // `guarded` catches.
        let result = guarded(selector, || unsafe { send0::<Id>(object, sel(selector)) });
        assert_eq!(
            result.unwrap_err(),
            DeviceError::Protocol("framebufferSurface raised an Objective-C exception".into())
        );
        let object = Object::retain(object).unwrap();
        assert!(!object.responds(c"framebufferSurface"));
        assert!(object.responds(c"length"));
        assert!(object.get(c"framebufferSurface").is_err());
        assert!(!object.conforms(c"SimScreen"));
    }
}
