use super::wrap_hresult;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    io,
    sync::{Arc, Mutex, Weak},
    time::Instant,
};
use winapi::shared::dxgi1_2::{
    IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
};
// USER handles use 32 bits; reserve a separate namespace for physical DXGI shapes.
pub const CURSOR_ID_FLAG: u64 = 1 << 63;
const CHANNELS: u32 = 4;
const BITS_PER_BYTE: u32 = 8;
const DEFAULT_DPI: f64 = 96.0;
#[derive(Clone)]
pub struct Shape {
    pub id: u64,
    pub scale: f64,
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub hotspot: (i32, i32),
    pub pixels: Vec<u8>,
}

impl Shape {
    fn new(info: DXGI_OUTDUPL_POINTER_SHAPE_INFO, pixels: Vec<u8>) -> io::Result<Self> {
        let (height, minimum_pitch) = match info.Type {
            DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME if info.Height % 2 == 0 => {
                (info.Height / 2, info.Width.div_ceil(BITS_PER_BYTE))
            }
            DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR
            | DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR => (
                info.Height,
                info.Width.checked_mul(CHANNELS).ok_or_else(invalid_shape)?,
            ),
            _ => return Err(invalid_shape()),
        };
        let length = (info.Pitch as usize).checked_mul(info.Height as usize);
        if info.Width == 0
            || height == 0
            || info.Width > i32::MAX as u32
            || info.Height > i32::MAX as u32
            || info.Pitch > i32::MAX as u32
            || info.Pitch < minimum_pitch
            || length != Some(pixels.len())
            || info.HotSpot.x < 0
            || info.HotSpot.x as u32 >= info.Width
            || info.HotSpot.y < 0
            || info.HotSpot.y as u32 >= height
        {
            return Err(invalid_shape());
        }
        let hotspot = (info.HotSpot.x, info.HotSpot.y);
        Ok(Self {
            id: 0,
            scale: 0.0,
            kind: info.Type,
            width: info.Width,
            height,
            pitch: info.Pitch,
            hotspot,
            pixels,
        })
    }

    fn with_scale(self, scale: f64) -> Self {
        let mut hash = DefaultHasher::new();
        (
            self.kind,
            self.width,
            self.height,
            self.pitch,
            self.hotspot,
            &self.pixels,
            scale.to_bits(),
        )
            .hash(&mut hash);
        Self {
            id: hash.finish() | CURSOR_ID_FLAG,
            scale,
            ..self
        }
    }
}

fn invalid_shape() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid DXGI cursor shape")
}

#[derive(Clone)]
pub enum Snapshot {
    Unavailable,
    Pending,
    Ready(Arc<Shape>),
    Failed(String),
}

struct State {
    updated: Instant,
    snapshot: Snapshot,
}

type SharedState = Arc<Mutex<State>>;

lazy_static::lazy_static! {
    static ref CAPTURES: Mutex<HashMap<usize, Vec<Weak<Mutex<State>>>>> =
        Mutex::new(HashMap::new());
}

pub fn snapshot(monitor: usize) -> Snapshot {
    let captures = CAPTURES.lock().unwrap();
    captures
        .get(&monitor)
        .into_iter()
        .flatten()
        .filter_map(Weak::upgrade)
        .map(|state| {
            let state = state.lock().unwrap();
            (state.updated, state.snapshot.clone())
        })
        .max_by_key(|(updated, _)| *updated)
        .map(|(_, snapshot)| snapshot)
        .unwrap_or(Snapshot::Unavailable)
}

pub fn shape(id: u64) -> Option<Arc<Shape>> {
    CAPTURES
        .lock()
        .unwrap()
        .values()
        .flatten()
        .filter_map(Weak::upgrade)
        .find_map(|state| match &state.lock().unwrap().snapshot {
            Snapshot::Ready(shape) if shape.id == id => Some(shape.clone()),
            _ => None,
        })
}

pub(super) struct Capture {
    monitor: usize,
    state: SharedState,
}

impl Capture {
    pub fn new(monitor: usize) -> Self {
        let state = Arc::new(Mutex::new(State {
            updated: Instant::now(),
            snapshot: Snapshot::Pending,
        }));
        let capture = Self { monitor, state };
        capture.activate();
        capture
    }

    pub fn activate(&self) {
        let mut captures = CAPTURES.lock().unwrap();
        let states = captures.entry(self.monitor).or_default();
        let own = Arc::downgrade(&self.state);
        if !states.iter().any(|state| state.ptr_eq(&own)) {
            states.push(own);
        }
    }

    pub fn deactivate(&self) {
        let mut captures = CAPTURES.lock().unwrap();
        if let Some(states) = captures.get_mut(&self.monitor) {
            let own = Arc::downgrade(&self.state);
            states.retain(|state| !state.ptr_eq(&own));
            if states.is_empty() {
                captures.remove(&self.monitor);
            }
        }
    }

    pub unsafe fn update(
        &self,
        duplication: *mut IDXGIOutputDuplication,
        frame: &DXGI_OUTDUPL_FRAME_INFO,
    ) {
        let snapshot = match self.read(duplication, frame) {
            Ok(Some(shape)) => Snapshot::Ready(Arc::new(shape)),
            Ok(None) => return,
            Err(error) => {
                hbb_common::log::error!("DXGI cursor capture failed: {error}");
                Snapshot::Failed(error.to_string())
            }
        };
        *self.state.lock().unwrap() = State {
            updated: Instant::now(),
            snapshot,
        };
    }

    unsafe fn read(
        &self,
        duplication: *mut IDXGIOutputDuplication,
        frame: &DXGI_OUTDUPL_FRAME_INFO,
    ) -> io::Result<Option<Shape>> {
        use windows::Win32::{
            Graphics::Gdi::HMONITOR,
            UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        };
        // The Flutter runner is per-monitor DPI aware, so this is the output DPI.
        let (mut x, mut y) = (0, 0);
        GetDpiForMonitor(
            HMONITOR(self.monitor as _),
            MDT_EFFECTIVE_DPI,
            &mut x,
            &mut y,
        )?;
        if x == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid monitor scale",
            ));
        }
        let scale = x as f64 / DEFAULT_DPI;
        if frame.PointerShapeBufferSize > 0 {
            return read(duplication, frame.PointerShapeBufferSize)
                .map(|shape| Some(shape.with_scale(scale)));
        }
        // A DPI change can leave a custom cursor's physical bitmap unchanged.
        Ok(match &self.state.lock().unwrap().snapshot {
            Snapshot::Ready(shape) if shape.scale != scale => {
                Some(shape.as_ref().clone().with_scale(scale))
            }
            _ => None,
        })
    }
}

unsafe fn read(duplication: *mut IDXGIOutputDuplication, size: u32) -> io::Result<Shape> {
    let mut pixels = vec![0; size as usize];
    let mut required = 0;
    let mut info = std::mem::zeroed();
    wrap_hresult((*duplication).GetFramePointerShape(
        size,
        pixels.as_mut_ptr().cast(),
        &mut required,
        &mut info,
    ))?;
    if required > size {
        return Err(invalid_shape());
    }
    pixels.truncate(required as usize);
    Shape::new(info, pixels)
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.deactivate();
    }
}

#[cfg(test)]
mod tests;
