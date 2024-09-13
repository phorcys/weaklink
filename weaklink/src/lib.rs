//! This crate documents the runtime API for weaklink stubs generated by `weaklink_build`.
//!
//! Access to the API is through static variables exposed in the generated stubs crate:
//! - A [`Library`] object, named according to the configuration in `weaklink_build::Config`.
//! - A [`Group`] object for each symbol group defined via `weaklink_build::Config::add_symbol_group()`.
//!
//! ## Example:
//! ```rust,ignore
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     stub::library.load_from("path/to/library")?;
//!
//!     stub::all.resolve()?; // Assuming you've created the "all" API group.
//!     use_api();
//!
//!     Ok(())
//! }
//! ```
//!
//! # Checked Mode
//! When the stub crate is compiled with the `checked` feature, API stubs will verify that the corresponding symbol
//! has been resolved before use. This is done by tracking the resolution state of each symbol on a per-thread basis.
//! As a result, there is a runtime overhead for each call, so it is recommended to disable this feature in release builds.
//!
//! This feature is intended to be used with [`Group::if_resolved()`]:
//! ```rust,ignore
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     stub::library.load_from("path/to/library")?;
//!
//!     stub::base_api.resolve()?; // Base API is always present in the library.
//!     use_base_api();
//!
//!     if stub::optional_api.if_resolved(|| {
//!         use_optional_api();
//!     }).is_err() {
//!         println!("Optional API not available.");
//!     }
//!
//!     // This will panic in checked mode!
//!     use_optional_api();
//!
//!     Ok(())
//! }
//! ```

pub use loading::{Address, DylibHandle};
use std::{
    cell::UnsafeCell,
    ffi::{CStr, CString},
    mem,
    panic::catch_unwind,
    path::Path,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

pub type Error = Box<dyn std::error::Error>;

pub mod loading;

#[cfg(feature = "checked")]
use std::{
    cell::RefCell,
    sync::{OnceLock, RwLock},
};
#[cfg(feature = "checked")]
use thread_local::ThreadLocal;

/// Represents a weakly linked dynamic library.
#[repr(C)]
pub struct Library {
    handle: AtomicUsize,
    dylib_names: &'static [&'static str],
    symbol_names: &'static [&'static CStr],
    symbol_table: &'static [Address],

    // Must initialize this stuff lazily, so we can have a const constructor.
    #[cfg(feature = "checked")]
    checked_state: OnceLock<CheckedState>,
}

#[cfg(feature = "checked")]
struct CheckedState {
    shadow_symbol_table: &'static [Address],
    global_asserted: RwLock<Box<[u8]>>,
    thread_asserted: ThreadLocal<RefCell<Box<[u8]>>>,
}

impl Library {
    #[doc(hidden)]
    pub const fn new(
        dylib_names: &'static [&'static str],
        symbol_names: &'static [&'static CStr],
        symbol_table: &'static [Address],
    ) -> Library {
        Library {
            handle: AtomicUsize::new(0),
            dylib_names,
            symbol_names,
            symbol_table,
            #[cfg(feature = "checked")]
            checked_state: OnceLock::new(),
        }
    }

    /// Load library with default name (configured at build time).
    pub fn load(&self) -> Result<DylibHandle, Error> {
        let raw_handle = self.handle.load(Ordering::Acquire);
        if raw_handle != 0 {
            return Err("Already loaded.".into());
        } else {
            for name in self.dylib_names {
                let cpath = CString::new(*name).unwrap();
                if let Ok(handle) = loading::load_library(&cpath) {
                    self.handle.store(handle.0, Ordering::Release);
                    return Ok(handle);
                }
            }
        }
        Err("Library not found.".into())
    }

    /// Load library from the specified path.
    pub fn load_from(&self, path: &Path) -> Result<DylibHandle, Error> {
        let raw_handle = self.handle.load(Ordering::Acquire);
        if raw_handle != 0 {
            Err("Already loaded.".into())
        } else {
            let cpath = CString::new(path.as_os_str().to_str().unwrap().as_bytes()).unwrap();
            match loading::load_library(&cpath) {
                Ok(handle) => {
                    self.handle.store(handle.0, Ordering::Release);
                    Ok(handle)
                }
                Err(err) => Err(err),
            }
        }
    }

    /// Sets the library handle directly.
    ///
    /// The handle may be obtained via [`loading::load_library`] or from platform-specific APIs.
    pub fn set_handle(&self, handle: DylibHandle) {
        self.handle.store(handle.0, Ordering::Release);
    }

    /// Returns the library handle if it is loaded, or previously set via `set_handle`.
    pub fn handle(&self) -> Option<DylibHandle> {
        let raw_handle = self.handle.load(Ordering::Acquire);
        if raw_handle != 0 {
            Some(DylibHandle(raw_handle))
        } else {
            None
        }
    }

    // Make sure the library is loaded, or panic.
    fn ensure_loaded(&self) -> DylibHandle {
        match self.handle() {
            Some(handle) => handle,
            None => match self.load() {
                Ok(handle) => handle,
                Err(err) => panic!("{}", err),
            },
        }
    }

    // Resolve symbol address.
    fn resolve_symbol_uncached(&self, sym_index: u32) -> Result<Address, Error> {
        let handle = self.ensure_loaded();
        let sym_name = self.symbol_names[sym_index as usize];
        loading::find_symbol(handle, sym_name)
    }

    // Resolve symbol address and update its entry in the symbol table.
    fn resolve_symbol(&self, sym_index: u32) -> Result<Address, Error> {
        unsafe {
            let entry = self.symbol_table_entry(sym_index);

            #[cfg(feature = "checked")]
            {
                let addr = entry.read();
                if addr != 0 {
                    return Ok(addr);
                }
            }

            let result = self.resolve_symbol_uncached(sym_index);
            if let Ok(address) = &result {
                entry.write(*address);
            }
            result
        }
    }

    // This function gets invoked by the lazy resolver when a symbol is called into.
    #[doc(hidden)]
    pub fn lazy_resolve(&self, sym_index: u32) -> Address {
        let result = catch_unwind(|| {
            self.check_asserted(sym_index);
            match self.resolve_symbol(sym_index) {
                Ok(sym_addr) => sym_addr,
                Err(err) => panic!("Symbol could not be resolved: {}", err),
            }
        });

        match result {
            Ok(address) => address,
            // Can't unwind since we can't guarantee anything about the context this is invoked in.
            Err(_) => {
                std::process::abort();
            }
        }
    }
}

#[cfg(not(feature = "checked"))]
impl Library {
    // Get a reference to a symbol pointer.
    unsafe fn symbol_table_entry(&self, sym_index: u32) -> *mut Address {
        let ptr: &UnsafeCell<Address> = mem::transmute(&self.symbol_table[0]);
        ptr.get().offset(sym_index as isize) as *mut Address
    }

    fn assert_resolved(&self, _sym_indices: &[u32]) {}

    fn deassert_resolved(&self, _sym_indices: &[u32]) {}

    fn global_assert_resolved(&self, _sym_indices: &[u32]) {}

    fn check_asserted(&self, _sym_index: u32) -> bool {
        true
    }
}

#[cfg(feature = "checked")]
impl Library {
    // In checked mode we do not update the real symbol table, because that would prevent
    // further callbacks on symbol use.  Instead, we cache addresses in the shadow table.
    unsafe fn symbol_table_entry(&self, sym_index: u32) -> *mut Address {
        let checked_state = self.get_checked_state();
        let ptr: &UnsafeCell<Address> = mem::transmute(&checked_state.shadow_symbol_table[0]);
        ptr.get().offset(sym_index as isize) as *mut Address
    }

    fn get_checked_state(&self) -> &CheckedState {
        self.checked_state.get_or_init(|| CheckedState {
            shadow_symbol_table: Box::leak(boxed_slice(self.symbol_table.len())),
            global_asserted: RwLock::new(boxed_slice(self.symbol_table.len())),
            thread_asserted: ThreadLocal::new(),
        })
    }

    fn assert_resolved(&self, sym_indices: &[u32]) {
        let checked_state = self.get_checked_state();
        let mut asserted = checked_state
            .thread_asserted
            .get_or(|| RefCell::new(boxed_slice(self.symbol_table.len())))
            .borrow_mut();
        for sym_index in sym_indices {
            asserted[*sym_index as usize] += 1;
        }
    }

    fn deassert_resolved(&self, sym_indices: &[u32]) {
        let checked_state = self.get_checked_state();
        let mut asserted = checked_state.thread_asserted.get().unwrap().borrow_mut();
        for sym_index in sym_indices {
            asserted[*sym_index as usize] -= 1;
        }
    }

    fn global_assert_resolved(&self, sym_indices: &[u32]) {
        let checked_state = self.get_checked_state();
        let mut global_asserted = checked_state.global_asserted.write().unwrap();
        for sym_index in sym_indices {
            global_asserted[*sym_index as usize] = 1;
        }
    }

    fn check_asserted(&self, sym_index: u32) {
        // Any failure below indicates that assert_resolved() hadn't been called.
        let fail = || -> ! {
            panic!(
                "Symbol {:?} was used without having been asserted as resolved.",
                self.symbol_names[sym_index as usize]
            );
        };

        let checked_state = self.checked_state.get().unwrap_or_else(|| fail());
        let global_asserted = checked_state.global_asserted.read().unwrap();
        if global_asserted[sym_index as usize] == 0 {
            let asserted = checked_state.thread_asserted.get().unwrap_or_else(|| fail()).borrow();
            if asserted[sym_index as usize] == 0 {
                fail();
            }
        }
    }
}

/// Represents symbol group defined at build time.
#[repr(C)]
pub struct Group {
    library: &'static Library,
    sym_indices: &'static [u32],
    status: AtomicU8,
}

const GROUP_STATUS_UNRESOLVED: u8 = 0;
const GROUP_STATUS_RESOLVED: u8 = 1;
const GROUP_STATUS_FAILED: u8 = 2;

impl Group {
    #[doc(hidden)]
    pub const fn new(library: &'static Library, sym_indices: &'static [u32]) -> Group {
        Group {
            library,
            sym_indices,
            status: AtomicU8::new(GROUP_STATUS_UNRESOLVED),
        }
    }

    /// Resolves the group's symbols if they haven't been resolved yet.
    ///
    /// In checked mode, the group will be permanently marked as resolved (for all threads).
    pub fn resolve(&self) -> bool {
        match self.status.load(Ordering::Acquire) {
            GROUP_STATUS_UNRESOLVED => {
                let result = self.resolve_uncached().is_ok();
                let status = match result {
                    true => {
                        self.library.global_assert_resolved(self.sym_indices);
                        GROUP_STATUS_RESOLVED
                    }
                    false => GROUP_STATUS_FAILED,
                };
                self.status.store(status, Ordering::Release);
                result
            }
            GROUP_STATUS_RESOLVED => true,
            GROUP_STATUS_FAILED | _ => false,
        }
    }

    /// Resolves the group's symbols if they haven't been resolved yet, then calls the provided closure if successful.
    ///
    /// When the stub crate is compiled with the `checked` feature, this function temporarily marks the group as resolved
    /// while the closure is executed, and unmarks it afterward. This ensures that the APIs are not used before resolution.
    ///
    /// See [checked mode](index.html#checked-mode) for more details.
    ///
    /// # Thread Safety
    /// The resolution states are tracked per thread, making this function thread-safe.  However,
    pub fn if_resolved<R>(&self, f: impl Fn() -> R) -> Result<R, Error> {
        if self.resolve() {
            self.library.assert_resolved(self.sym_indices);
            let result = f();
            self.library.deassert_resolved(self.sym_indices);
            Ok(result)
        } else {
            Err("Symbol group could not be resolved".into())
        }
    }

    /// Attempt to resolve all symbols in the group, unconditionally.
    ///
    /// It is recommended to use [`resolve`](Group::resolve) or [`if_resolved`](Group::if_resolved) instead,
    /// which cache the resolution status.   However, this function may be useful if you want to learn the
    /// cause of a resolution failure.
    pub fn resolve_uncached(&self) -> Result<(), Error> {
        for sym_index in self.sym_indices {
            if let Err(err) = self.library.resolve_symbol(*sym_index) {
                return Err(err);
            }
        }
        Ok(())
    }
}

#[cfg(feature = "checked")]
fn boxed_slice<T: Copy + Default>(size: usize) -> Box<[T]> {
    let mut v = Vec::<T>::with_capacity(size);
    v.resize(size, Default::default());
    v.into_boxed_slice()
}
