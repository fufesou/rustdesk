use super::*;
use winapi::shared::windef::POINT;

#[test]
fn cursor_state_follows_capture_lifetime_and_gdi_switches() {
    const MONITOR: usize = usize::MAX;
    let first = Capture::new(MONITOR);
    let second = Capture::new(MONITOR);
    first.deactivate();
    assert!(matches!(snapshot(MONITOR), Snapshot::Pending));
    drop(second);
    assert!(matches!(snapshot(MONITOR), Snapshot::Unavailable));
    first.activate();
    first.activate();
    assert!(matches!(snapshot(MONITOR), Snapshot::Pending));
    drop(first);
    assert!(matches!(snapshot(MONITOR), Snapshot::Unavailable));
}

#[test]
fn cursor_keeps_physical_hotspot_and_both_monochrome_planes() {
    let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
        Type: DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
        Width: 64,
        Height: 128,
        Pitch: 8,
        HotSpot: POINT { x: 31, y: 29 },
    };
    let pixels = vec![0xa5; 1024];
    let shape = Shape::new(info, pixels.clone()).unwrap().with_scale(1.0);
    assert_eq!(
        (shape.width, shape.height, shape.hotspot),
        (64, 64, (31, 29))
    );
    assert_eq!(shape.pixels, pixels);
    let scaled = shape.clone().with_scale(2.0);
    assert_eq!(scaled.scale, 2.0);
    assert_ne!(shape.id, scaled.id);
    assert_eq!(shape.id, scaled.with_scale(1.0).id);
    assert!(Shape::new(info, vec![0; 512]).is_err());
    let mut changed = info;
    changed.HotSpot.y += 1;
    assert_ne!(
        shape.id,
        Shape::new(changed, pixels).unwrap().with_scale(1.0).id
    );
}
