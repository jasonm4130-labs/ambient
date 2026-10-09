//! The push-to-talk key: ⌥Space, held anywhere.
//!
//! A Carbon hot key (`RegisterEventHotKey`) rather than an event tap or an
//! `NSEvent` global monitor. The system matches the chord itself and hands
//! this process a pressed and a released event, so the app never sees any
//! other keystroke and needs neither Accessibility nor Input Monitoring
//! permission; the other two routes read every key and need one of those.
//! Registration was measured on macOS 26.6.2: ⌥Space registers with status 0
//! and no permission prompt. macOS 15.0 and 15.1 refused Option-only chords
//! for a while (FB15163561, fixed in 15.2), so a refusal is reported in the
//! menu rather than assumed away.
//!
//! The chord is held for the whole time talking mode is on, and macOS gives
//! it to this app alone: ⌥Space stops typing a non-breaking space anywhere
//! else. That is why talking mode is off by default.

use anyhow::{bail, Result};
use std::cell::RefCell;
use std::ffi::c_void;

type OSStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUPP = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;
    fn GetEventKind(event: EventRef) -> u32;
}

const fn four(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const K_EVENT_CLASS_KEYBOARD: u32 = four(b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_HOT_KEY_RELEASED: u32 = 6;
/// `kVK_Space`.
const KEY_SPACE: u32 = 49;
/// `optionKey`.
const OPTION: u32 = 1 << 11;

/// How the chord is written wherever the app names it.
pub const CHORD: &str = "⌥Space";

/// What a press (`true`) or release (`false`) of the chord calls.
type OnKey = Box<dyn Fn(bool)>;

thread_local! {
    /// Carbon delivers hot key events on the main thread's run loop, which
    /// is the only thread that registers one, so this never crosses threads.
    static ON_KEY: RefCell<Option<OnKey>> = const { RefCell::new(None) };
    static HANDLER: RefCell<Option<EventHandlerRef>> = const { RefCell::new(None) };
}

extern "C" fn on_hot_key(
    _call: EventHandlerCallRef,
    event: EventRef,
    _data: *mut c_void,
) -> OSStatus {
    // SAFETY: `event` is the live event Carbon is dispatching to us.
    let pressed = match unsafe { GetEventKind(event) } {
        K_EVENT_HOT_KEY_PRESSED => true,
        K_EVENT_HOT_KEY_RELEASED => false,
        _ => return 0,
    };
    ON_KEY.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(pressed);
        }
    });
    0
}

/// ⌥Space, registered until dropped.
pub struct HotKey {
    key: EventHotKeyRef,
}

impl HotKey {
    /// Register ⌥Space and call `on_key(true)` when it goes down and
    /// `on_key(false)` when it comes up. Main thread only.
    pub fn register(on_key: impl Fn(bool) + 'static) -> Result<Self> {
        ON_KEY.with(|f| *f.borrow_mut() = Some(Box::new(on_key)));
        let installed = HANDLER.with(|h| h.borrow().is_some());
        // SAFETY: plain Carbon calls on the main thread; the handler is a
        // `'static` function and the type list outlives the call.
        unsafe {
            if !installed {
                let kinds = [
                    EventTypeSpec {
                        event_class: K_EVENT_CLASS_KEYBOARD,
                        event_kind: K_EVENT_HOT_KEY_PRESSED,
                    },
                    EventTypeSpec {
                        event_class: K_EVENT_CLASS_KEYBOARD,
                        event_kind: K_EVENT_HOT_KEY_RELEASED,
                    },
                ];
                let mut handler: EventHandlerRef = std::ptr::null_mut();
                let st = InstallEventHandler(
                    GetApplicationEventTarget(),
                    on_hot_key,
                    kinds.len() as u32,
                    kinds.as_ptr(),
                    std::ptr::null_mut(),
                    &mut handler,
                );
                if st != 0 {
                    bail!("could not listen for {CHORD}: InstallEventHandler returned {st}");
                }
                HANDLER.with(|h| *h.borrow_mut() = Some(handler));
            }
            let mut key: EventHotKeyRef = std::ptr::null_mut();
            let id = EventHotKeyID {
                signature: four(b"ambt"),
                id: 1,
            };
            let st = RegisterEventHotKey(
                KEY_SPACE,
                OPTION,
                id,
                GetApplicationEventTarget(),
                0,
                &mut key,
            );
            match st {
                0 => Ok(Self { key }),
                // eventHotKeyExistsErr: another app holds the chord.
                -9878 => bail!("another app already uses {CHORD}"),
                _ => bail!("macOS refused {CHORD} (status {st})"),
            }
        }
    }
}

impl Drop for HotKey {
    fn drop(&mut self) {
        // SAFETY: `key` came from a successful `RegisterEventHotKey`.
        unsafe { UnregisterEventHotKey(self.key) };
        ON_KEY.with(|f| *f.borrow_mut() = None);
    }
}
