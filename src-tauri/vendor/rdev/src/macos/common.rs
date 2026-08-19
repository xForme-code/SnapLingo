#![allow(clippy::upper_case_acronyms)]
use crate::macos::keyboard::Keyboard;
use crate::rdev::{Button, Event, EventType};
use cocoa::base::id;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, EventField};
use lazy_static::lazy_static;
use std::convert::TryInto;
use std::os::raw::c_void;
use std::sync::Mutex;
use std::time::SystemTime;

use crate::macos::keycodes::key_from_code;

pub type CFMachPortRef = *const c_void;
pub type CFIndex = u64;
pub type CFAllocatorRef = id;
pub type CFRunLoopSourceRef = id;
pub type CFRunLoopRef = id;
pub type CFRunLoopMode = id;
pub type CGEventTapProxy = id;
pub type CGEventRef = CGEvent;

// https://developer.apple.com/documentation/coregraphics/cgeventtapplacement?language=objc
pub type CGEventTapPlacement = u32;
#[allow(non_upper_case_globals)]
pub const kCGHeadInsertEventTap: u32 = 0;

// https://developer.apple.com/documentation/coregraphics/cgeventtapoptions?language=objc
#[allow(non_upper_case_globals)]
#[repr(u32)]
pub enum CGEventTapOption {
    #[cfg(feature = "unstable_grab")]
    Default = 0,
    ListenOnly = 1,
}

pub static mut LAST_FLAGS: CGEventFlags = CGEventFlags::CGEventFlagNull;
lazy_static! {
    pub static ref KEYBOARD_STATE: Mutex<Keyboard> = Mutex::new(Keyboard::new().unwrap());
}

// https://developer.apple.com/documentation/coregraphics/cgeventmask?language=objc
pub type CGEventMask = u64;
// [SnapLingo 补丁] 只监听鼠标事件。
//
// 原版掩码包含 KeyDown：回调里会为它调用 TSM 输入法 API 解析按键字符，
// 而新版 macOS 要求这些 API 必须在主线程调用，事件钩子线程一调用就触发
// dispatch_assert_queue 断言，整个进程直接被 SIGTRAP 杀掉。
// SnapLingo 只消费鼠标按下/抬起/移动，键盘事件从掩码里去掉后，
// 崩溃路径彻底消失，顺便也不再截获用户键盘输入。
#[allow(non_upper_case_globals)]
pub const kCGEventMaskForAllEvents: u64 = (1 << CGEventType::LeftMouseDown as u64)
    + (1 << CGEventType::LeftMouseUp as u64)
    + (1 << CGEventType::MouseMoved as u64);

#[cfg(target_os = "macos")]
#[link(name = "Cocoa", kind = "framework")]
extern "C" {
    #[allow(improper_ctypes)]
    pub fn CGEventTapCreate(
        tap: CGEventTapLocation,
        place: CGEventTapPlacement,
        options: CGEventTapOption,
        eventsOfInterest: CGEventMask,
        callback: QCallback,
        user_info: id,
    ) -> CFMachPortRef;
    pub fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        tap: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub fn CFRunLoopRun();

    pub static kCFRunLoopCommonModes: CFRunLoopMode;

}
pub type QCallback = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    _type: CGEventType,
    cg_event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

pub unsafe fn convert(
    _type: CGEventType,
    cg_event: &CGEvent,
    keyboard_state: &mut Keyboard,
) -> Option<Event> {
    let option_type = match _type {
        CGEventType::LeftMouseDown => Some(EventType::ButtonPress(Button::Left)),
        CGEventType::LeftMouseUp => Some(EventType::ButtonRelease(Button::Left)),
        CGEventType::RightMouseDown => Some(EventType::ButtonPress(Button::Right)),
        CGEventType::RightMouseUp => Some(EventType::ButtonRelease(Button::Right)),
        CGEventType::MouseMoved => {
            let point = cg_event.location();
            Some(EventType::MouseMove {
                x: point.x,
                y: point.y,
            })
        }
        CGEventType::KeyDown => {
            let code = cg_event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            Some(EventType::KeyPress(key_from_code(code.try_into().ok()?)))
        }
        CGEventType::KeyUp => {
            let code = cg_event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            Some(EventType::KeyRelease(key_from_code(code.try_into().ok()?)))
        }
        CGEventType::FlagsChanged => {
            let code = cg_event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            let code = code.try_into().ok()?;
            let flags = cg_event.get_flags();
            if flags < LAST_FLAGS {
                LAST_FLAGS = flags;
                Some(EventType::KeyRelease(key_from_code(code)))
            } else {
                LAST_FLAGS = flags;
                Some(EventType::KeyPress(key_from_code(code)))
            }
        }
        CGEventType::ScrollWheel => {
            let delta_y =
                cg_event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
            let delta_x =
                cg_event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
            Some(EventType::Wheel { delta_x, delta_y })
        }
        _ => None,
    };
    if let Some(event_type) = option_type {
        // [SnapLingo 补丁] 不再为 KeyPress 解析字符：create_string_for_key 会走
        // TSM 输入法 API，在非主线程调用会触发断言崩溃。掩码已剔除键盘事件，
        // 这里一并移除是双保险。
        let _ = keyboard_state;
        let name = None;
        return Some(Event {
            event_type,
            time: SystemTime::now(),
            name,
        });
    }
    None
}
