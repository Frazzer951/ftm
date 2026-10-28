//! An Objective-C exception raised by AppKit while tao's `-[NSApplication sendEvent:]` override dispatches an event
//! must reach the handler in `-[NSApplication run]`, which reports it and keeps running. Through an `extern "C"`
//! override it aborts the app instead (#25).

#[cfg(target_os = "macos")]
fn main() {
    macos::exception_under_send_event_does_not_abort();
    println!("send_event_exceptions: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSException, NSPoint, NSRect, NSSize, NSString};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use tao::event::{Event, StartCause};
    use tao::event_loop::{ControlFlow, EventLoop};
    use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
    use tao::platform::run_return::EventLoopExtRunReturn;

    static THROWN: AtomicBool = AtomicBool::new(false);

    define_class!(
        /// Stands in for the AppKit code that asserts while handling an event (NSCampoLightweightUIController).
        #[unsafe(super(NSWindow))]
        #[thread_kind = MainThreadOnly]
        struct ThrowingWindow;

        impl ThrowingWindow {
            #[unsafe(method(sendEvent:))]
            fn send_event(&self, _event: &NSEvent) {
                THROWN.store(true, Ordering::SeqCst);
                // SAFETY: exceptionWithName:reason:userInfo: takes an NSString, an optional NSString and an optional
                // NSDictionary, and gets live objects of those types.
                let exception = unsafe {
                    NSException::exceptionWithName_reason_userInfo(
                        &NSString::from_str("NSInternalInconsistencyException"),
                        Some(&NSString::from_str("thrown under sendEvent:")),
                        None,
                    )
                };
                objc2::exception::throw(NSException::into_exception(exception))
            }
        }
    );

    pub fn exception_under_send_event_does_not_abort() {
        let mtm = MainThreadMarker::new().expect("harness = false runs this on the main thread");
        let mut event_loop = EventLoop::new();
        // No Dock icon, menu bar or focus change: the test never shows a window either.
        event_loop.set_activation_policy(ActivationPolicy::Prohibited);
        event_loop.set_activate_ignoring_other_apps(false);

        let mut window: Option<Retained<ThrowingWindow>> = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        event_loop.run_return(|event, _, control_flow| {
            *control_flow = ControlFlow::WaitUntil(deadline);
            match event {
                Event::NewEvents(StartCause::Init) => {
                    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 10.0));
                    // SAFETY: initWithContentRect:styleMask:backing:defer: is NSWindow's designated initializer and
                    // gets arguments of the types it takes. defer:NO gives the window a number to target below.
                    let throwing: Retained<ThrowingWindow> = unsafe {
                        msg_send![ThrowingWindow::alloc(mtm), initWithContentRect: frame, styleMask: NSWindowStyleMask::Borderless, backing: NSBackingStoreType::Buffered, defer: false]
                    };
                    // NSApplication's sendEvent: forwards an application-defined event to its window's sendEvent:.
                    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                        NSEventType::ApplicationDefined,
                        NSPoint::new(0.0, 0.0),
                        NSEventModifierFlags::empty(),
                        0.0,
                        throwing.windowNumber(),
                        None,
                        0,
                        0,
                        0,
                    )
                    .expect("an application-defined event");
                    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
                    window = Some(throwing);
                }
                Event::NewEvents(StartCause::ResumeTimeReached { .. }) => *control_flow = ControlFlow::Exit,
                _ => {}
            }
        });
        assert!(THROWN.load(Ordering::SeqCst), "the posted event never reached ThrowingWindow");
    }
}
