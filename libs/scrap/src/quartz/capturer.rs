use std::ptr;

use block::{Block, ConcreteBlock};
use hbb_common::{libc::c_void, log};
use std::sync::{Arc, Mutex};

use super::config::Config;
use super::display::Display;
use super::ffi::*;
use super::frame::Frame;

pub struct Capturer {
    stream: CGDisplayStreamRef,
    queue: DispatchQueue,

    width: usize,
    height: usize,
    format: PixelFormat,
    display: Display,
    stopped: Arc<Mutex<StopState>>,
}

#[derive(Default)]
struct StopState {
    stopped: bool,
    retired: Option<(CGDisplayStreamRef, DispatchQueue)>,
}

impl StopState {
    fn release_if_stopped(&mut self) {
        if !self.stopped {
            return;
        }
        if let Some((stream, queue)) = self.retired.take() {
            // Release on the serial queue after the current callback has returned.
            let context = Box::into_raw(Box::new((stream, queue)));
            unsafe { dispatch_async_f(queue, context.cast(), release_stream) };
        }
    }
}

unsafe extern "C" fn release_stream(context: *mut c_void) {
    let (stream, queue) = *Box::from_raw(context as *mut (CGDisplayStreamRef, DispatchQueue));
    CFRelease(stream);
    dispatch_release(queue);
    log::info!("Released stopped display stream {:p}", stream);
}

impl Capturer {
    pub fn new<F: Fn(Frame) + 'static>(
        display: Display,
        width: usize,
        height: usize,
        format: PixelFormat,
        config: Config,
        handler: F,
    ) -> Result<Capturer, CGError> {
        let stopped = Arc::new(Mutex::new(StopState::default()));
        let cloned_stopped = stopped.clone();
        let handler: FrameAvailableHandler = ConcreteBlock::new(move |status, _, surface, _| {
            use self::CGDisplayStreamFrameStatus::*;
            if status == Stopped {
                let mut lock = cloned_stopped.lock().unwrap();
                lock.stopped = true;
                lock.release_if_stopped();
                return;
            }
            if status == FrameComplete {
                handler(unsafe { Frame::new(surface) });
            }
        })
        .copy();

        let queue = unsafe {
            dispatch_queue_create(
                b"quadrupleslap.scrap\0".as_ptr() as *const i8,
                ptr::null_mut(),
            )
        };

        let stream = unsafe {
            let config = config.build();
            let stream = CGDisplayStreamCreateWithDispatchQueue(
                display.id(),
                width,
                height,
                format,
                config,
                queue,
                &*handler as *const Block<_, _> as *const c_void,
            );
            CFRelease(config);
            stream
        };

        match unsafe { CGDisplayStreamStart(stream) } {
            CGError::Success => Ok(Capturer {
                stream,
                queue,
                width,
                height,
                format,
                display,
                stopped,
            }),
            x => Err(x),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn format(&self) -> PixelFormat {
        self.format
    }
    pub fn display(&self) -> Display {
        self.display
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        let result = unsafe { CGDisplayStreamStop(self.stream) };
        if result != CGError::Success {
            log::error!(
                "Failed to stop display {} stream {:p}: {:?}",
                self.display.id(),
                self.stream,
                result
            );
        }
        let mut state = self.stopped.lock().unwrap();
        log::info!(
            "Retiring display {} stream {:p}; awaiting stop notification: {}",
            self.display.id(),
            self.stream,
            !state.stopped
        );
        // Stop may return before or after its callback. Both must finish before release.
        state.retired = Some((self.stream, self.queue));
        state.release_if_stopped();
    }
}
