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
    ffi::{CStr, CString, c_char, c_void},
    os::unix::ffi::OsStrExt as _,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    ptr::{NonNull, null, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use block2::{Block, ManualBlockEncoding, RcBlock};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use futures_util::future::BoxFuture;
use objc2::{
    exception,
    ffi::{self, objc_method_description},
    rc::{Retained, autoreleasepool},
    runtime::{AnyClass, AnyObject, AnyProtocol, Bool, Sel},
};
use objc2_core_foundation::{CFRetained, CFType};
use objc2_foundation::{NSError, NSString, NSUUID};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::{
    guard::{GuardResult, Owner, SymbolResolver, probe_with},
    hid::{
        DIGITIZER_SERVICE, HidAction, HidError, HidSink, IndigoMessage, PURPLE_MESSAGE_SIZE,
        SINGLE_TOUCH_LEN, TouchPhase, XpcValue, dtu_message, indigo_message, liveness_message,
        purple_orientation_message, single_touch,
    },
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

/// A C function's address, called with its §0 signature through [`Symbol::as_fn`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Symbol(NonNull<c_void>);

// SAFETY: a code address is immutable and valid on every thread.
unsafe impl Send for Symbol {}
// SAFETY: as above.
unsafe impl Sync for Symbol {}

impl Symbol {
    /// The function as `F`.
    ///
    /// # Safety
    ///
    /// `F` must be an `unsafe extern "C" fn` type matching the symbol's real signature.
    unsafe fn as_fn<F: Copy>(self) -> F {
        const { assert!(size_of::<F>() == size_of::<*mut c_void>()) };
        // SAFETY: a function pointer and a data pointer have one size on Apple platforms;
        // the caller names the signature.
        unsafe { std::mem::transmute_copy(&self.0.as_ptr()) }
    }
}

/// The C functions live input calls (spec §0, S1).
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
    pub hid: HidSymbols,
}

#[cfg(test)]
impl CoreSim {
    /// A `CoreSim` whose symbols must never be called, for guard tests.
    pub(crate) fn fake() -> Arc<Self> {
        let symbol = Symbol(NonNull::dangling());
        Arc::new(Self {
            developer_dir: PathBuf::from("/nonexistent"),
            hid: HidSymbols {
                mouse_event: symbol,
                button: symbol,
                hid_arbitrary: symbol,
                endpoint_create_mach_port: symbol,
                connection_from_endpoint: symbol,
                enable_sim2host: symbol,
            },
        })
    }
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
/// As [`send0`], with four arguments.
unsafe fn send4<A, B, C, D>(receiver: Id, selector: Sel, a: A, b: B, c: C, d: D) {
    // SAFETY: the caller guarantees the signature.
    unsafe {
        msg_send_fn::<unsafe extern "C-unwind" fn(Id, Sel, A, B, C, D)>()(
            receiver, selector, a, b, c, d,
        );
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

    /// Takes ownership of an object returned at +1 (`init…`); `None` for nil.
    fn owned(object: Id) -> Option<Self> {
        // SAFETY: `object` is nil or a live object the callee returned retained.
        unsafe { Retained::from_raw(object) }.map(Self)
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
        if !self.responds(c"count") {
            return Err(missing(c"count"));
        }
        if !self.responds(c"objectAtIndex:") {
            return Err(missing(c"objectAtIndex:"));
        }
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

/// Whether `object`'s real class (its `isa`, read without sending a message, so a proxy
/// cannot answer for its target) is `class` or a subclass of it.
fn is_instance_of(object: Id, class: &CStr) -> bool {
    let (Some(object), Some(class)) = (NonNull::new(object), AnyClass::get(class)) else {
        return false;
    };
    // SAFETY: `object` is a live object the callee handed us; reading its class sends no
    // message.
    let mut current = Some(unsafe { object.as_ref() }.class());
    while let Some(candidate) = current {
        if std::ptr::eq(candidate, class) {
            return true;
        }
        current = candidate.superclass();
    }
    false
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
        if !is_instance_of(uuid.ptr(), c"NSUUID") {
            tracing::warn!("a simulator's UDID is not an NSUUID");
            continue;
        }
        // SAFETY: the object's own class was just checked to be `NSUUID`.
        let uuid = unsafe { Retained::cast_unchecked::<NSUUID>(uuid.0) };
        let Ok(text) = guarded(c"UUIDString", || uuid.UUIDString().to_string()) else {
            continue;
        };
        if text.eq_ignore_ascii_case(udid) {
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
    /// Retains `object` when it is a real IOSurface (toll-free bridged with
    /// `IOSurfaceRef`). Anything else, a remote proxy included, is refused before any
    /// CoreFoundation call; an exception raised anyway is caught and refuses it too.
    fn retain(object: Id) -> Option<Self> {
        if !is_instance_of(object, c"IOSurface") {
            return None;
        }
        let object = NonNull::new(object)?;
        guarded(c"CFRetain", || {
            // SAFETY: `object`'s own class is `IOSurface` (checked above without a message),
            // which is toll-free bridged with `IOSurfaceRef`, so it is a CFType;
            // `downcast` confirms the CF type id.
            let object = unsafe { CFRetained::retain(object.cast::<CFType>()) };
            object.downcast::<IOSurfaceRef>().ok().map(Self)
        })
        .ok()
        .flatten()
    }

    /// The surface's portrait size.
    pub(crate) fn size(&self) -> (u32, u32) {
        let width = u32::try_from(self.0.width()).unwrap_or(0);
        let height = u32::try_from(self.0.height()).unwrap_or(0);
        (width, height)
    }

    /// The surface itself, which the encoder wraps in a `CVPixelBuffer`.
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
/// block without one ("Block is missing signature field"). Every other block BiBCode hands
/// a framework (XPC handlers and barriers, the legacy HID completion) carries one too.
/// `void (^)(void)`: frame callbacks and XPC barriers.
struct VoidBlock;
// SAFETY: `void (^)(void)` is `v8@?0` on 64-bit, matching `Arguments` and `Return`.
unsafe impl ManualBlockEncoding for VoidBlock {
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

/// `void (^)(id)`: properties callbacks, XPC event and reply handlers (an `xpc_object_t`
/// is an object), and the legacy HID completion (`NSError *`).
struct ObjectBlock;
// SAFETY: `void (^)(id)` is `v16@?0@8` on 64-bit, matching `Arguments`.
unsafe impl ManualBlockEncoding for ObjectBlock {
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
    /// Set once a callback handed over something that is not an IOSurface.
    refused_surface: AtomicBool,
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
            refused_surface: AtomicBool::new(false),
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
            RcBlock::with_encoding::<_, _, _, VoidBlock>(move || state.tick())
        };
        let surfaces = {
            let state = state.clone();
            RcBlock::with_encoding::<_, _, _, SurfacesBlock>(move |framebuffer: Id, _masked: Id| {
                match IoSurface::retain(framebuffer) {
                    Some(surface) => state.set_surface(surface),
                    None => {
                        if !state.refused_surface.swap(true, Ordering::Relaxed) {
                            tracing::warn!(
                                "the simulator handed over a framebuffer that is not an IOSurface; ignoring it"
                            );
                        }
                    }
                }
            })
        };
        let properties = {
            let state = state.clone();
            RcBlock::with_encoding::<_, _, _, ObjectBlock>(move |properties: Id| {
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
        let registered = guarded(register, || unsafe {
            send5(
                descriptor.ptr(),
                sel(register),
                uuid.ptr(),
                queue_ptr,
                frame,
                surfaces,
                properties,
            );
        });
        let screen = Self {
            descriptor,
            uuid,
            _queue: queue,
            state,
            detached: AtomicBool::new(false),
        };
        // The send may have raised after CoreSimulator stored the callbacks: dropping the
        // screen unregisters them, best effort.
        registered?;
        Ok(screen)
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

// ---------------------------------------------------------------------------------------------
// Input: DTUHID over in-process XPC, legacy Indigo, and Purple rotation (spec §0, S1–S2)

/// An opaque XPC type descriptor (`_xpc_type_s`); only its address is compared.
#[repr(C)]
struct XpcType {
    _private: [u8; 0],
}

type Xpc = *mut c_void;

// SAFETY: libxpc and the Mach calls are part of libSystem, which every macOS process links;
// these are their public C signatures.
unsafe extern "C" {
    static _xpc_type_dictionary: XpcType;
    static _xpc_type_error: XpcType;
    fn xpc_dictionary_create(keys: *const *const c_char, values: *const Xpc, count: usize) -> Xpc;
    fn xpc_dictionary_set_uint64(dictionary: Xpc, key: *const c_char, value: u64);
    fn xpc_dictionary_set_double(dictionary: Xpc, key: *const c_char, value: f64);
    fn xpc_dictionary_set_bool(dictionary: Xpc, key: *const c_char, value: bool);
    fn xpc_dictionary_set_string(dictionary: Xpc, key: *const c_char, value: *const c_char);
    fn xpc_dictionary_set_value(dictionary: Xpc, key: *const c_char, value: Xpc);
    fn xpc_get_type(object: Xpc) -> *const XpcType;
    fn xpc_release(object: Xpc);
    fn xpc_connection_set_event_handler(connection: Xpc, handler: *const Block<dyn Fn(Id)>);
    fn xpc_connection_resume(connection: Xpc);
    fn xpc_connection_cancel(connection: Xpc);
    fn xpc_connection_send_message(connection: Xpc, message: Xpc);
    fn xpc_connection_send_barrier(connection: Xpc, barrier: *const Block<dyn Fn()>);
    fn xpc_connection_send_message_with_reply(
        connection: Xpc,
        message: Xpc,
        reply_queue: *mut c_void,
        handler: *const Block<dyn Fn(Id)>,
    );
    fn mach_msg(
        message: *mut c_void,
        option: i32,
        send_size: u32,
        receive_size: u32,
        receive_name: u32,
        timeout: u32,
        notify: u32,
    ) -> i32;
    fn mach_port_deallocate(task: u32, name: u32) -> i32;
    static mach_task_self_: u32;
    fn mach_absolute_time() -> u64;
}

const MACH_SEND_MSG: i32 = 0x1;
const MACH_SEND_TIMEOUT: i32 = 0x10;
/// How long `mach_msg` may wait to queue the Purple message.
const PURPLE_SEND_TIMEOUT_MS: u32 = 2000;
/// How long the liveness barrier's reply may take (spec §0: idb's 4 s).
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(4);
/// How long to wait between liveness attempts.
const LIVENESS_BACKOFF: Duration = Duration::from_secs(4);
const LIVENESS_ATTEMPTS: u32 = 5;
/// How long one DTUHID message's barrier, or one legacy completion, may take.
const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// An XPC object this code owns (+1), released on drop.
struct XpcObject(NonNull<c_void>);

// SAFETY: XPC objects are reference counted atomically and libxpc allows them on any
// thread; connections are thread-safe and the dictionaries are not changed once sent.
unsafe impl Send for XpcObject {}
// SAFETY: as above.
unsafe impl Sync for XpcObject {}

impl XpcObject {
    fn new(object: Xpc) -> Result<Self, DeviceError> {
        NonNull::new(object)
            .map(Self)
            .ok_or_else(|| protocol_error("libxpc returned NULL"))
    }

    fn ptr(&self) -> Xpc {
        self.0.as_ptr()
    }
}

impl Drop for XpcObject {
    fn drop(&mut self) {
        // SAFETY: this code owns one reference.
        unsafe { xpc_release(self.0.as_ptr()) };
    }
}

/// Whether `object` (borrowed from a handler) is an XPC object of `kind`.
fn xpc_is(object: Id, kind: *const XpcType) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: `object` is a live XPC object the handler received; reading its type does
    // not retain it.
    std::ptr::eq(unsafe { xpc_get_type(object.cast()) }, kind)
}

/// Builds an XPC dictionary from `entries`.
fn xpc_dictionary(entries: &[(&'static CStr, XpcValue)]) -> Result<XpcObject, DeviceError> {
    // SAFETY: an empty dictionary: no keys or values to read.
    let dictionary = XpcObject::new(unsafe { xpc_dictionary_create(null(), null(), 0) })?;
    for (key, value) in entries {
        let (dictionary, key) = (dictionary.ptr(), key.as_ptr());
        // SAFETY: `dictionary` is live, `key` and string values are NUL-terminated and
        // copied by libxpc, and a nested value is retained by the dictionary.
        unsafe {
            match value {
                XpcValue::U64(value) => xpc_dictionary_set_uint64(dictionary, key, *value),
                XpcValue::Double(value) => xpc_dictionary_set_double(dictionary, key, *value),
                XpcValue::Bool(value) => xpc_dictionary_set_bool(dictionary, key, *value),
                XpcValue::Str(value) => xpc_dictionary_set_string(dictionary, key, value.as_ptr()),
                XpcValue::Dict(entries) => {
                    let nested = xpc_dictionary(entries)?;
                    xpc_dictionary_set_value(dictionary, key, nested.ptr());
                }
            }
        }
    }
    Ok(dictionary)
}

fn xpc_message(message: &XpcValue) -> Result<XpcObject, DeviceError> {
    match message {
        XpcValue::Dict(entries) => xpc_dictionary(entries),
        _ => Err(protocol_error("a DTUHID message must be a dictionary")),
    }
}

/// A one-shot completion that a framework block fires on its own thread.
fn completion<T: Send + 'static>() -> (impl Fn(T) + Send + Sync + 'static, oneshot::Receiver<T>) {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    let fire = move |value: T| {
        if let Some(sender) = lock(&sender).take() {
            let _ = sender.send(value);
        }
    };
    (fire, receiver)
}

/// One send right this task holds to a simulator service, deallocated on drop.
struct SendRight(u32);

impl Drop for SendRight {
    fn drop(&mut self) {
        // SAFETY: this code holds one user reference to the right; `lookup` added it.
        let status = unsafe { mach_port_deallocate(mach_task_self_, self.0) };
        if status != 0 {
            tracing::warn!(status, "could not release a simulator service port");
        }
    }
}

/// `-[SimDevice lookup:error:]` for `service`. Inside a drained autorelease pool the call
/// leaves this task exactly one send right, which [`SendRight`] releases. Blocking: it is a
/// round trip to CoreSimulatorService.
fn lookup(device: &Object, service: &CStr) -> Result<SendRight, DeviceError> {
    let selector = c"lookup:error:";
    if !device.responds(selector) {
        return Err(missing(selector));
    }
    autoreleasepool(|_| {
        let name = NSString::from_str(&service.to_string_lossy());
        let name: Id = Retained::as_ptr(&name).cast::<AnyObject>().cast_mut();
        let mut error: Id = null_mut();
        // SAFETY: `-lookup:error:` is `I32@0:8@16^@24` (checked by the guard): an NSString
        // and an error out-pointer, returning a `mach_port_t`.
        let port = guarded(selector, || unsafe {
            send2::<Id, *mut Id, u32>(device.ptr(), sel(selector), name, &raw mut error)
        })?;
        if port == 0 {
            return Err(error_from(selector, error));
        }
        Ok(SendRight(port))
    })
}

/// A connection to the guest's `dtuhidd` (spec §0, the DTUHID path).
struct DtuConnection {
    connection: XpcObject,
    /// Set by the event handler on any connection error, and by [`cancel`](Self::cancel).
    dead: Arc<AtomicBool>,
    cancelled: AtomicBool,
    /// Released after the connection, which holds its own reference.
    _port: SendRight,
}

type EndpointFromPort = unsafe extern "C" fn(u32, u64, u64) -> Xpc;
type ConnectionFromEndpoint = unsafe extern "C" fn(Xpc) -> Xpc;
type EnableSim2Host = unsafe extern "C" fn(Xpc);

impl DtuConnection {
    /// Looks the digitizer service up and connects in §0's order: endpoint, connection,
    /// sim2host, event handler, resume. Blocking (the lookup); proves nothing about
    /// `dtuhidd` until [`liveness`](Self::liveness) answers.
    fn open(sim: &CoreSim, device: &Object) -> Result<Self, DeviceError> {
        let port = lookup(device, DIGITIZER_SERVICE)?;
        // SAFETY: §0 records these signatures; the guard resolved each symbol.
        let (endpoint_from_port, from_endpoint, enable_sim2host) = unsafe {
            (
                sim.hid
                    .endpoint_create_mach_port
                    .as_fn::<EndpointFromPort>(),
                sim.hid
                    .connection_from_endpoint
                    .as_fn::<ConnectionFromEndpoint>(),
                sim.hid.enable_sim2host.as_fn::<EnableSim2Host>(),
            )
        };
        // SAFETY: `port.0` is a send right this task holds; the endpoint does not take it.
        let endpoint = XpcObject::new(unsafe { endpoint_from_port(port.0, 0, 0) })?;
        // SAFETY: `endpoint` is a live endpoint; the connection retains what it needs.
        let connection = XpcObject::new(unsafe { from_endpoint(endpoint.ptr()) })?;
        drop(endpoint);
        let dead = Arc::new(AtomicBool::new(false));
        let handler = {
            let dead = dead.clone();
            RcBlock::with_encoding::<_, _, _, ObjectBlock>(move |event: Id| {
                if xpc_is(event, &raw const _xpc_type_error) {
                    dead.store(true, Ordering::SeqCst);
                }
            })
        };
        let handler: *const Block<dyn Fn(Id)> = &*handler;
        // SAFETY: `connection` is live and not yet resumed; libxpc copies the handler,
        // which captures only an atomic flag.
        unsafe {
            enable_sim2host(connection.ptr());
            xpc_connection_set_event_handler(connection.ptr(), handler);
            xpc_connection_resume(connection.ptr());
        }
        Ok(Self {
            connection,
            dead,
            cancelled: AtomicBool::new(false),
            _port: port,
        })
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Sends §0's barrier key-up with a reply and waits [`LIVENESS_TIMEOUT`] for it.
    async fn liveness(&self) -> Result<(), String> {
        let reply = self.send_liveness().map_err(|error| error.to_string())?;
        match tokio::time::timeout(LIVENESS_TIMEOUT, reply).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err("the barrier was answered with an error".to_owned()),
            Ok(Err(_)) => Err("the barrier's reply handler was dropped".to_owned()),
            Err(_) => Err("no barrier reply in time".to_owned()),
        }
    }

    /// Sends the liveness barrier; the receiver says whether a dictionary came back.
    fn send_liveness(&self) -> Result<oneshot::Receiver<bool>, DeviceError> {
        let message = xpc_message(&liveness_message())?;
        let (fire, reply) = completion::<bool>();
        let handler = RcBlock::with_encoding::<_, _, _, ObjectBlock>(move |reply: Id| {
            fire(xpc_is(reply, &raw const _xpc_type_dictionary));
        });
        let handler: *const Block<dyn Fn(Id)> = &*handler;
        // SAFETY: a live connection and message; a NULL queue runs the handler on the
        // connection's queue; libxpc copies the handler, which only fires a channel.
        unsafe {
            xpc_connection_send_message_with_reply(
                self.connection.ptr(),
                message.ptr(),
                null_mut(),
                handler,
            );
        }
        Ok(reply)
    }

    /// Sends one message, then a barrier, and waits for the barrier (§0's send sequence).
    async fn send(&self, message: &XpcValue) -> Result<(), String> {
        if self.is_dead() {
            return Err("the DTUHID connection closed".to_owned());
        }
        let done = self
            .send_with_barrier(message)
            .map_err(|error| error.to_string())?;
        match tokio::time::timeout(SEND_TIMEOUT, done).await {
            Ok(Ok(())) if !self.is_dead() => Ok(()),
            Ok(_) => Err("the DTUHID connection closed".to_owned()),
            Err(_) => Err("the DTUHID barrier did not run in time".to_owned()),
        }
    }

    fn send_with_barrier(&self, message: &XpcValue) -> Result<oneshot::Receiver<()>, DeviceError> {
        let message = xpc_message(message)?;
        let (fire, done) = completion::<()>();
        let barrier = RcBlock::with_encoding::<_, _, _, VoidBlock>(move || fire(()));
        let barrier: *const Block<dyn Fn()> = &*barrier;
        // SAFETY: a live connection and message; libxpc retains the message and copies the
        // barrier, which only fires a channel.
        unsafe {
            xpc_connection_send_message(self.connection.ptr(), message.ptr());
            xpc_connection_send_barrier(self.connection.ptr(), barrier);
        }
        Ok(done)
    }

    /// Cancels the connection once; pending barriers and replies then run.
    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        self.dead.store(true, Ordering::SeqCst);
        // SAFETY: a live connection; cancelling twice is prevented above.
        unsafe { xpc_connection_cancel(self.connection.ptr()) };
    }
}

impl Drop for DtuConnection {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CgPair {
    a: f64,
    b: f64,
}

type MouseEvent =
    unsafe extern "C" fn(*const CgPair, *const CgPair, u32, u64, CgPair, u32) -> *mut u8;
type ButtonEvent = unsafe extern "C" fn(i32, i32, i32) -> *mut u8;
type HidArbitrary = unsafe extern "C" fn(i32, u32, u32, i32) -> *mut u8;

/// `IndigoHIDMessageFor…` direction: 1 down, 2 up.
fn direction(down: bool) -> i32 {
    if down { 1 } else { 2 }
}

/// A `malloc`'d Indigo message, freed on drop unless handed to the client.
struct IndigoBuffer(NonNull<u8>);

impl IndigoBuffer {
    fn new(message: *mut u8) -> Result<Self, DeviceError> {
        NonNull::new(message)
            .map(Self)
            .ok_or_else(|| protocol_error("an Indigo message builder returned NULL"))
    }

    /// Hands the buffer to a callee that frees it (`freeWhenDone:YES`).
    fn into_raw(self) -> *mut u8 {
        let pointer = self.0.as_ptr();
        std::mem::forget(self);
        pointer
    }
}

impl Drop for IndigoBuffer {
    fn drop(&mut self) {
        // SAFETY: the buffer came from `malloc`/`calloc` and nobody else owns it.
        unsafe { libc::free(self.0.as_ptr().cast()) };
    }
}

/// SimulatorKit's `SimDeviceLegacyHIDClient`: the fallback for touch and buttons.
struct LegacyHid {
    client: Object,
    queue: DispatchRetained<DispatchQueue>,
    mouse: MouseEvent,
    button: ButtonEvent,
    arbitrary: HidArbitrary,
}

impl LegacyHid {
    /// `-[SimDeviceLegacyHIDClient initWithDevice:error:]`. Blocking.
    fn new(sim: &CoreSim, device: &Object) -> Result<Self, DeviceError> {
        let class = AnyClass::get(c"SimulatorKit.SimDeviceLegacyHIDClient")
            .ok_or_else(|| missing(c"SimulatorKit.SimDeviceLegacyHIDClient"))?;
        let class: Id = std::ptr::from_ref(class).cast::<AnyObject>().cast_mut();
        let init = c"initWithDevice:error:";
        let client = autoreleasepool(|_| {
            // SAFETY: `+alloc` takes nothing and returns an uninitialised instance (+1).
            let allocated = guarded(c"alloc", || unsafe { send0::<Id>(class, sel(c"alloc")) })?;
            if allocated.is_null() {
                return Err(nil(c"alloc"));
            }
            let mut error: Id = null_mut();
            // SAFETY: `-initWithDevice:error:` is `@32@0:8@16^@24` (checked by the guard);
            // it consumes `allocated` and returns the client at +1, or nil.
            let client = guarded(init, || unsafe {
                send2::<Id, *mut Id, Id>(allocated, sel(init), device.ptr(), &raw mut error)
            })?;
            Object::owned(client).ok_or_else(|| error_from(init, error))
        })?;
        // SAFETY: §0 records these builders' signatures; the guard resolved each symbol.
        let (mouse, button, arbitrary) = unsafe {
            (
                sim.hid.mouse_event.as_fn::<MouseEvent>(),
                sim.hid.button.as_fn::<ButtonEvent>(),
                sim.hid.hid_arbitrary.as_fn::<HidArbitrary>(),
            )
        };
        Ok(Self {
            client,
            queue: DispatchQueue::new("dev.bibcode.ios-legacy-hid", DispatchQueueAttr::SERIAL),
            mouse,
            button,
            arbitrary,
        })
    }

    /// The `malloc`'d message for `message`.
    fn build(&self, message: &IndigoMessage) -> Result<IndigoBuffer, DeviceError> {
        match *message {
            IndigoMessage::Touch { x, y, down } => {
                let point = CgPair { a: x, b: y };
                // SAFETY: §0's builder: a point, no second point, target 0x32, NSEventType
                // 1 (down) or 2 (up), a unit size so the point is a ratio, edge 0. It
                // returns a `malloc`'d message.
                let builder = IndigoBuffer::new(unsafe {
                    (self.mouse)(
                        &point,
                        null(),
                        0x32,
                        u64::from(direction(down).unsigned_abs()),
                        CgPair { a: 1.0, b: 1.0 },
                        0,
                    )
                })?;
                // SAFETY: `malloc_size` reads the allocation's size from its zone.
                let size = unsafe { libc::malloc_size(builder.0.as_ptr().cast()) };
                if size < 0xA0 {
                    return Err(protocol_error("the Indigo touch message is too small"));
                }
                // SAFETY: the allocation holds at least 0xA0 bytes, checked above.
                let source = unsafe { std::slice::from_raw_parts(builder.0.as_ptr(), 0xA0) };
                // SAFETY: reads the host's monotonic clock.
                let message = single_touch(source, x, y, unsafe { mach_absolute_time() });
                drop(builder);
                // SAFETY: a fresh zeroed allocation the client will `free`.
                let buffer =
                    IndigoBuffer::new(unsafe { libc::calloc(1, SINGLE_TOUCH_LEN) }.cast())?;
                // SAFETY: `buffer` holds `SINGLE_TOUCH_LEN` bytes, as does `message`.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        message.as_ptr(),
                        buffer.0.as_ptr(),
                        SINGLE_TOUCH_LEN,
                    );
                }
                Ok(buffer)
            }
            IndigoMessage::Button { source, down } => {
                // SAFETY: §0's builder: source, direction, target 0x33; `malloc`'d.
                IndigoBuffer::new(unsafe { (self.button)(source, direction(down), 0x33) })
            }
            IndigoMessage::Consumer { usage, down } => {
                // SAFETY: §0's builder: target 0x32, usage page, usage, direction; `malloc`'d.
                IndigoBuffer::new(unsafe { (self.arbitrary)(0x32, 0x0C, usage, direction(down)) })
            }
        }
    }

    /// Sends `message` and waits for its completion.
    async fn send(&self, message: &IndigoMessage) -> Result<(), String> {
        let done = self.submit(message).map_err(|error| error.to_string())?;
        match tokio::time::timeout(SEND_TIMEOUT, done).await {
            Ok(Ok(None)) => Ok(()),
            Ok(Ok(Some(error))) => Err(error),
            Ok(Err(_)) => Err("the legacy HID completion was dropped".to_owned()),
            Err(_) => Err("the legacy HID send did not complete in time".to_owned()),
        }
    }

    /// Hands `message` to the client; the receiver gets the completion's error, if any.
    fn submit(
        &self,
        message: &IndigoMessage,
    ) -> Result<oneshot::Receiver<Option<String>>, DeviceError> {
        let buffer = self.build(message)?;
        let (fire, done) = completion::<Option<String>>();
        let handler = RcBlock::with_encoding::<_, _, _, ObjectBlock>(move |error: Id| {
            let failure = (!error.is_null()).then(|| {
                autoreleasepool(|_| {
                    if is_instance_of(error, c"NSError") {
                        // SAFETY: the object's own class was just checked to be `NSError`.
                        let error = unsafe { &*error.cast::<NSError>() };
                        error.localizedDescription().to_string()
                    } else {
                        "an unknown error".to_owned()
                    }
                })
            });
            fire(failure);
        });
        let handler: *const Block<dyn Fn(Id)> = &*handler;
        let queue: Id = DispatchRetained::as_ptr(&self.queue).as_ptr().cast();
        let selector = c"sendWithMessage:freeWhenDone:completionQueue:completion:";
        if !self.client.responds(selector) {
            return Err(missing(selector));
        }
        let pointer = buffer.into_raw();
        // SAFETY: `v44@0:8^{IndigoHIDMessageStruct=…}16B24@28@?36`: the message, which
        // the client frees (`freeWhenDone:YES`), a queue and a completion it copies. If
        // this raises, the message may or may not have been freed, so it is leaked.
        guarded(selector, || unsafe {
            send4::<*mut u8, Bool, Id, *const Block<dyn Fn(Id)>>(
                self.client.ptr(),
                sel(selector),
                pointer,
                Bool::YES,
                queue,
                handler,
            );
        })?;
        Ok(done)
    }
}

/// Sends the Purple orientation `value` to the simulator's `PurpleWorkspacePort`. Blocking:
/// a lookup round trip and `mach_msg` with a 2 s send timeout.
fn purple_rotate(device: &Object, value: u32) -> Result<(), DeviceError> {
    let port = lookup(device, c"PurpleWorkspacePort")?;
    let mut message = purple_orientation_message(port.0, value);
    // SAFETY: `message` is a 112-byte, 4-byte-aligned `mach_msg_header_t` and body whose
    // remote port is a send right this task holds (`COPY_SEND`, so it is kept); only a
    // send is asked for, with a timeout.
    let status = unsafe {
        mach_msg(
            message.as_mut_ptr().cast(),
            MACH_SEND_MSG | MACH_SEND_TIMEOUT,
            PURPLE_MESSAGE_SIZE,
            0,
            0,
            PURPLE_SEND_TIMEOUT_MS,
            0,
        )
    };
    drop(port);
    if status != 0 {
        tracing::warn!(status, "the simulator's Purple port refused a rotation");
        return Err(protocol_error(format!("mach_msg returned {status:#x}")));
    }
    Ok(())
}

/// The DTUHID connection's state.
enum Dtu {
    /// A handshake is running.
    Connecting,
    Live(Arc<DtuConnection>),
    /// The last handshake failed; the next key starts another.
    Down,
}

/// Which path a touch gesture went down on; its moves and lift follow it.
#[derive(Clone)]
enum InputPath {
    Dtu(Arc<DtuConnection>),
    Legacy,
}

/// Live input, keys and rotation for one booted simulator: DTUHID first, legacy Indigo for
/// touch and buttons while DTUHID is not live (spec §5).
pub(crate) struct HidClient {
    inner: Arc<HidInner>,
}

struct HidInner {
    sim: Arc<CoreSim>,
    device: Object,
    dtu: Mutex<Dtu>,
    legacy: tokio::sync::Mutex<Option<Arc<LegacyHid>>>,
    touch: Mutex<Option<InputPath>>,
    released: AtomicBool,
    /// Ends the handshake.
    cancel: CancellationToken,
    runtime: tokio::runtime::Handle,
}

impl HidClient {
    /// Finds the booted `udid` and starts the DTUHID handshake on `runtime` in the
    /// background. Blocking (the device lookup), so call it off the async runtime.
    pub(crate) fn start(
        sim: Arc<CoreSim>,
        udid: &str,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, DeviceError> {
        let device = autoreleasepool(|_| find_device(&sim, udid))?;
        let inner = Arc::new(HidInner {
            sim,
            device,
            dtu: Mutex::new(Dtu::Connecting),
            legacy: tokio::sync::Mutex::new(None),
            touch: Mutex::new(None),
            released: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            runtime: runtime.clone(),
        });
        runtime.spawn(handshake(inner.clone()));
        Ok(Self { inner })
    }
}

/// Connects to `dtuhidd` and waits for its barrier reply: [`LIVENESS_ATTEMPTS`] attempts of
/// [`LIVENESS_TIMEOUT`], [`LIVENESS_BACKOFF`] apart. `dtuhidd` is demand-launched and may
/// abort early in boot while its port still exists, so a port alone proves nothing.
async fn handshake(inner: Arc<HidInner>) {
    for attempt in 1..=LIVENESS_ATTEMPTS {
        if attempt > 1 {
            tokio::select! {
                () = inner.cancel.cancelled() => return,
                () = tokio::time::sleep(LIVENESS_BACKOFF) => {}
            }
        }
        let opener = inner.clone();
        let opened =
            tokio::task::spawn_blocking(move || DtuConnection::open(&opener.sim, &opener.device))
                .await;
        let connection = match opened {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                tracing::debug!(attempt, %error, "could not connect to the simulator's HID service");
                continue;
            }
            Err(error) => {
                tracing::debug!(attempt, %error, "the HID service lookup did not finish");
                continue;
            }
        };
        let answered = tokio::select! {
            () = inner.cancel.cancelled() => return,
            answered = connection.liveness() => answered,
        };
        match answered {
            Ok(()) => {
                let mut dtu = lock(&inner.dtu);
                if inner.released.load(Ordering::SeqCst) {
                    return;
                }
                *dtu = Dtu::Live(Arc::new(connection));
                tracing::info!(attempt, "simulator HID service is ready");
                return;
            }
            Err(reason) => {
                tracing::debug!(attempt, %reason, "the simulator's HID service did not answer");
            }
        }
    }
    let mut dtu = lock(&inner.dtu);
    if matches!(*dtu, Dtu::Connecting) {
        *dtu = Dtu::Down;
    }
    drop(dtu);
    tracing::warn!(
        "the simulator's HID service did not answer; touch uses the legacy path and keys are refused"
    );
}

impl HidInner {
    /// The live DTUHID connection; a dead one is dropped and a new handshake starts.
    fn dtu(self: &Arc<Self>) -> Option<Arc<DtuConnection>> {
        let mut dtu = lock(&self.dtu);
        match &*dtu {
            Dtu::Live(connection) if !connection.is_dead() => return Some(connection.clone()),
            Dtu::Connecting => return None,
            Dtu::Live(_) | Dtu::Down => {}
        }
        if self.released.load(Ordering::SeqCst) {
            return None;
        }
        *dtu = Dtu::Connecting;
        drop(dtu);
        tracing::debug!("reconnecting to the simulator's HID service");
        self.runtime.spawn(handshake(self.clone()));
        None
    }

    /// The legacy client, created on first use.
    async fn legacy(&self, this: &Arc<Self>) -> Result<Arc<LegacyHid>, HidError> {
        let mut legacy = self.legacy.lock().await;
        if let Some(client) = &*legacy {
            return Ok(client.clone());
        }
        if self.released.load(Ordering::SeqCst) {
            return Err(HidError::Released);
        }
        let creator = this.clone();
        let created =
            tokio::task::spawn_blocking(move || LegacyHid::new(&creator.sim, &creator.device))
                .await
                .map_err(|error| HidError::Failed(error.to_string()))?
                .map_err(|error| HidError::Failed(error.to_string()))?;
        if self.released.load(Ordering::SeqCst) {
            return Err(HidError::Released);
        }
        let created = Arc::new(created);
        *legacy = Some(created.clone());
        Ok(created)
    }

    /// The path for `action`, or [`HidError::Released`] once released.
    fn path(self: &Arc<Self>, action: &HidAction) -> Result<InputPath, HidError> {
        if self.released.load(Ordering::SeqCst) {
            return Err(HidError::Released);
        }
        Ok(self.path_unreleased(action))
    }

    /// A touch's moves and lift follow the path its down took.
    fn path_unreleased(self: &Arc<Self>, action: &HidAction) -> InputPath {
        let current = || self.dtu().map_or(InputPath::Legacy, InputPath::Dtu);
        let HidAction::Touch { phase, .. } = action else {
            return current();
        };
        let mut touch = lock(&self.touch);
        match phase {
            TouchPhase::Down => {
                let path = current();
                *touch = Some(path.clone());
                path
            }
            TouchPhase::Move => touch.clone().unwrap_or_else(current),
            TouchPhase::Up => touch.take().unwrap_or_else(current),
        }
    }

    async fn send(self: &Arc<Self>, action: HidAction) -> Result<(), HidError> {
        if self.released.load(Ordering::SeqCst) {
            return Err(HidError::Released);
        }
        match self.path(&action)? {
            InputPath::Dtu(connection) => {
                let message = dtu_message(&action)
                    .ok_or_else(|| HidError::Failed("a paste is not a HID message".to_owned()))?;
                connection.send(&message).await.map_err(|reason| {
                    connection.dead.store(true, Ordering::SeqCst);
                    HidError::Failed(reason)
                })
            }
            InputPath::Legacy => {
                let Some(message) = indigo_message(&action) else {
                    return Err(HidError::KeyboardUnavailable);
                };
                self.legacy(self)
                    .await?
                    .send(&message)
                    .await
                    .map_err(HidError::Failed)
            }
        }
    }
}

impl Drop for HidClient {
    /// Ends a handshake still running for a launch that never reached `Ready`.
    fn drop(&mut self) {
        self.inner.released.store(true, Ordering::SeqCst);
        self.inner.cancel.cancel();
    }
}

impl HidSink for HidClient {
    fn keyboard_ready(&self) -> bool {
        !self.inner.released.load(Ordering::SeqCst) && self.inner.dtu().is_some()
    }

    fn send(&self, action: HidAction) -> BoxFuture<'_, Result<(), HidError>> {
        Box::pin(self.inner.send(action))
    }

    fn rotate(&self, value: u32) -> BoxFuture<'_, Result<(), HidError>> {
        Box::pin(async move {
            if self.inner.released.load(Ordering::SeqCst) {
                return Err(HidError::Released);
            }
            let inner = self.inner.clone();
            tokio::task::spawn_blocking(move || purple_rotate(&inner.device, value))
                .await
                .map_err(|error| HidError::Failed(error.to_string()))?
                .map_err(|error| HidError::Failed(error.to_string()))
        })
    }

    fn release(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.inner.released.store(true, Ordering::SeqCst);
            self.inner.cancel.cancel();
            let dtu = std::mem::replace(&mut *lock(&self.inner.dtu), Dtu::Down);
            if let Dtu::Live(connection) = dtu {
                connection.cancel();
            }
            lock(&self.inner.touch).take();
            self.inner.legacy.lock().await.take();
        })
    }
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
    fn only_a_real_iosurface_is_retained_as_one() {
        let string = NSString::from_str("not a surface");
        let string: Id = Retained::as_ptr(&string).cast::<AnyObject>().cast_mut();
        assert!(IoSurface::retain(string).is_none());
        assert!(IoSurface::retain(null_mut()).is_none());
        assert!(is_instance_of(string, c"NSString"), "a subclass counts");
        assert!(!is_instance_of(string, c"NSUUID"));
        let uuid = NSUUID::new();
        let uuid: Id = Retained::as_ptr(&uuid).cast::<AnyObject>().cast_mut();
        assert!(is_instance_of(uuid, c"NSUUID"));
    }

    // SAFETY: libxpc's public getters, in libSystem.
    unsafe extern "C" {
        fn xpc_dictionary_get_value(dictionary: Xpc, key: *const c_char) -> Xpc;
        fn xpc_dictionary_get_uint64(dictionary: Xpc, key: *const c_char) -> u64;
        fn xpc_dictionary_get_double(dictionary: Xpc, key: *const c_char) -> f64;
        fn xpc_dictionary_get_bool(dictionary: Xpc, key: *const c_char) -> bool;
        fn xpc_dictionary_get_string(dictionary: Xpc, key: *const c_char) -> *const c_char;
        fn xpc_dictionary_get_count(dictionary: Xpc) -> usize;
    }

    #[test]
    fn dtuhid_messages_become_typed_xpc_dictionaries() {
        let action = HidAction::Touch {
            phase: TouchPhase::Up,
            x: 0.25,
            y: 0.75,
        };
        let message = xpc_message(&dtu_message(&action).unwrap()).unwrap();
        let root = message.ptr();
        // SAFETY: reads from live dictionaries this test owns; returned values are borrowed.
        unsafe {
            assert_eq!(xpc_dictionary_get_count(root), 4);
            let string = |key: &CStr| CStr::from_ptr(xpc_dictionary_get_string(root, key.as_ptr()));
            assert_eq!(string(c"messageType"), c"IndigoDigitizerEvent");
            assert_eq!(string(c"featureIdentifier"), DIGITIZER_SERVICE);
            assert!(!xpc_dictionary_get_bool(root, c"isBarrier".as_ptr()));
            let payload = xpc_dictionary_get_value(root, c"payload".as_ptr());
            assert!(xpc_is(payload.cast(), &raw const _xpc_type_dictionary));
            assert_eq!(xpc_dictionary_get_uint64(payload, c"eventType".as_ptr()), 2);
            let point = xpc_dictionary_get_value(payload, c"pointOne".as_ptr());
            assert_eq!(xpc_dictionary_get_double(point, c"x".as_ptr()), 0.25);
            assert_eq!(xpc_dictionary_get_double(point, c"y".as_ptr()), 0.75);
        }
        assert!(xpc_message(&XpcValue::U64(1)).is_err());
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
