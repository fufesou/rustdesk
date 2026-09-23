use super::{FlutterSession, SessionID};

pub(crate) fn refresh_capture(session: &FlutterSession, session_id: &SessionID) {
    let mut handlers = session.ui_handler.session_handlers.write().unwrap();
    let Some(handler) = handlers.get_mut(session_id) else {
        return;
    };
    // Read the current topology instead of indices from an older UI event.
    let count = session.ui_handler.peer_info.read().unwrap().displays.len();
    handler.displays = (0..count).collect();
    let has_other_rgba_reader = handlers
        .iter()
        .any(|(id, handler)| id != session_id && handler.displays == [0]);
    // Keep borrowed pixels alive and request the recovery frame after their acknowledgement.
    let wait_for_rgba = session
        .ui_handler
        .display_rgbas
        .write()
        .unwrap()
        .get_mut(&0)
        .is_some_and(|rgba| {
            // The calling window copies pixels before processing another UI event.
            if count == 1 && !has_other_rgba_reader {
                rgba.valid = false;
            }
            rgba.refresh_on_ack = count == 1 && rgba.valid;
            rgba.refresh_on_ack
        });
    // All displays includes every valid selection in the other windows.
    // Switching displays here would also restore a saved custom resolution.
    session.capture_displays(vec![], vec![], (0..count as i32).collect());
    if !wait_for_rgba {
        for display in 0..count {
            session.refresh_video(display as i32);
        }
    }
}

pub(crate) fn refresh_after_rgba(session: &FlutterSession, display: usize) {
    let pending = session
        .ui_handler
        .display_rgbas
        .write()
        .unwrap()
        .get_mut(&display)
        .is_some_and(|rgba| std::mem::take(&mut rgba.refresh_on_ack));
    if pending {
        session.refresh_video(display as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client::Data,
        flutter::{FlutterHandler, RgbaData, SessionHandler},
        ui_session_interface::{InvokeUiSession, Session},
    };
    use base::message_proto::{misc, DisplayInfo};
    use hbb_common::{get_version_number, tokio::sync::mpsc};
    use std::sync::Arc;

    const BORROWED_PIXEL: [u8; 4] = [17, 34, 51, 255];
    const NEXT_PIXEL: [u8; 4] = [68, 85, 102, 255];

    fn two_window_session() -> (FlutterSession, SessionID, mpsc::UnboundedReceiver<Data>) {
        let session = Arc::new(Session::<FlutterHandler>::default());
        let (sender, receiver) = mpsc::unbounded_channel();
        *session.sender.write().unwrap() = Some(sender);
        session.lc.write().unwrap().version = get_version_number("1.4.9");
        let all_displays = SessionID::new_v4();
        for (id, displays) in [(all_displays, vec![0, 1]), (SessionID::new_v4(), vec![0])] {
            session.ui_handler.session_handlers.write().unwrap().insert(
                id,
                SessionHandler {
                    displays,
                    ..Default::default()
                },
            );
        }
        session.ui_handler.peer_info.write().unwrap().displays = vec![DisplayInfo::default()];
        session.ui_handler.display_rgbas.write().unwrap().insert(
            0,
            RgbaData {
                data: BORROWED_PIXEL.to_vec(),
                valid: true,
                ..Default::default()
            },
        );
        (session, all_displays, receiver)
    }

    #[test]
    fn hotplug_refresh_preserves_another_windows_borrowed_rgba() {
        let (session, all_displays, mut receiver) = two_window_session();
        // The other window has fetched this pointer but has not acknowledged it.
        let borrowed = session.ui_handler.get_rgba(0);
        assert!(!borrowed.is_null());

        refresh_capture(&session, &all_displays);
        assert_eq!(session.ui_handler.get_rgba(0), borrowed);
        let Data::Message(capture) = receiver.try_recv().unwrap() else {
            panic!("Expected capture subscriptions");
        };
        assert_eq!(capture.misc().capture_displays().set, vec![0]);
        assert!(receiver.try_recv().is_err());

        let mut next = scrap::ImageRgb::new(scrap::ImageFormat::ARGB, crate::get_dst_align_rgba());
        next.raw = NEXT_PIXEL.to_vec();
        session.ui_handler.on_rgba_soft_render(0, &mut next);
        assert_eq!(session.ui_handler.get_rgba(0), borrowed);
        assert_eq!(next.raw, NEXT_PIXEL);
        assert_eq!(
            session.ui_handler.display_rgbas.read().unwrap()[&0].data,
            BORROWED_PIXEL
        );

        session.ui_handler.next_rgba(0);
        refresh_after_rgba(&session, 0);
        let Data::Message(refresh) = receiver.try_recv().unwrap() else {
            panic!("Expected the deferred video refresh");
        };
        assert!(matches!(
            refresh.misc().union,
            Some(misc::Union::RefreshVideoDisplay(0))
        ));
        assert!(receiver.try_recv().is_err());
        session.ui_handler.on_rgba_soft_render(0, &mut next);
        assert_eq!(
            session.ui_handler.display_rgbas.read().unwrap()[&0].data,
            NEXT_PIXEL
        );
    }

    #[test]
    fn hotplug_refresh_only_updates_capture() {
        let session = Arc::new(Session::<FlutterHandler>::default());
        let (sender, mut receiver) = mpsc::unbounded_channel();
        *session.sender.write().unwrap() = Some(sender);
        session.lc.write().unwrap().version = get_version_number("1.4.9");
        let id = SessionID::new_v4();
        session
            .ui_handler
            .session_handlers
            .write()
            .unwrap()
            .insert(id, SessionHandler::default());

        for count in [2, 1, 0, 1, 3, 2] {
            session.ui_handler.peer_info.write().unwrap().displays =
                vec![DisplayInfo::default(); count];
            session.ui_handler.display_rgbas.write().unwrap().insert(
                0,
                RgbaData {
                    valid: true,
                    ..Default::default()
                },
            );
            refresh_capture(&session, &id);
            assert_eq!(
                session.ui_handler.session_handlers.read().unwrap()[&id].displays,
                (0..count).collect::<Vec<_>>()
            );
            let Data::Message(message) = receiver.try_recv().unwrap() else {
                panic!("Expected capture subscriptions");
            };
            assert!(matches!(
                message.misc().union,
                Some(misc::Union::CaptureDisplays(_))
            ));
            assert_eq!(
                message.misc().capture_displays().set,
                (0..count as i32).collect::<Vec<_>>()
            );
            for display in 0..count {
                let Data::Message(message) = receiver.try_recv().unwrap() else {
                    panic!("Expected a video refresh");
                };
                assert!(matches!(message.misc().union,
                    Some(misc::Union::RefreshVideoDisplay(index)) if index == display as i32));
            }
            assert!(receiver.try_recv().is_err());
            if count == 1 {
                assert!(!session.ui_handler.display_rgbas.read().unwrap()[&0].valid);
            }
        }
    }
}
