use super::*;

#[test]
fn named_color_query_reads_the_windows_adapter_and_target() {
    // CtlCaptureColor from DisplayProfile.h, including the padding before Generation.
    let mut reply = [0u8; 96];
    reply[72..76].copy_from_slice(&123u32.to_le_bytes());
    reply[76..80].copy_from_slice(&(-45i32).to_le_bytes());
    reply[80..84].copy_from_slice(&67u32.to_le_bytes());
    let (adapter, target) = display_target::target_id(&reply).unwrap();
    assert_eq!((adapter.LowPart, adapter.HighPart, target), (123, -45, 67));
}

#[test]
fn disconnect_retries_failed_monitors_and_releases_the_others() {
    let mut monitors = BTreeMap::from([(0, false), (1, true), (2, false)]);
    let result = detach_monitors(&mut monitors, &[0, 1, 2], |fails| {
        if *fails {
            bail!("driver busy")
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert_eq!(monitors.keys().copied().collect::<Vec<_>>(), [1]);
    detach_monitors(&mut monitors, &[1], |_| Ok(())).unwrap();
    assert!(monitors.is_empty());
}
