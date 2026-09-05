use hbb_common::{
    log,
    message_proto::{message, video_frame, EncodedVideoFrames, Message},
    protobuf::Message as _,
    tokio::time::Instant,
};

pub(super) struct VideoSendDiagnostics {
    test_id: Option<&'static str>,
    sequence: u64,
}

pub(super) struct VideoSendStart<'a> {
    pub(super) conn_id: i32,
    pub(super) enqueued_at: &'a Instant,
    pub(super) message: &'a Message,
    pub(super) queued_after_recv: usize,
    pub(super) ack_required: bool,
}

pub(super) struct VideoSendTrace {
    test_id: &'static str,
    conn_id: i32,
    sequence: u64,
    enqueued_at: Instant,
    send_started: Instant,
}

impl VideoSendDiagnostics {
    pub(super) fn new(test_id: Option<&'static str>) -> Self {
        Self {
            test_id,
            sequence: 0,
        }
    }

    pub(super) fn start(&mut self, context: VideoSendStart<'_>) -> Option<VideoSendTrace> {
        let test_id = self.test_id?;
        let frame = extract_video_send_diag(context.message)?;
        self.sequence = self.sequence.wrapping_add(1);
        log::debug!(
            target: "video_diag",
            "[VIDEO_DIAG] test={} side=host event=video_send_start conn={} seq={} display={} codec={} keyframe={} packets={} payload_bytes={} wire_bytes={} first_pts={:?} last_pts={:?} queue_age_ms={} queued_after_recv={} ack_required={}",
            test_id,
            context.conn_id,
            self.sequence,
            frame.display,
            frame.codec,
            frame.keyframe,
            frame.packet_count,
            frame.payload_bytes,
            frame.wire_bytes,
            frame.first_pts,
            frame.last_pts,
            context.enqueued_at.elapsed().as_millis(),
            context.queued_after_recv,
            context.ack_required,
        );
        Some(VideoSendTrace {
            test_id,
            conn_id: context.conn_id,
            sequence: self.sequence,
            enqueued_at: context.enqueued_at.to_owned(),
            send_started: Instant::now(),
        })
    }
}

impl VideoSendTrace {
    pub(super) fn complete(self, queued_after_send: usize, success: bool) {
        log::debug!(
            target: "video_diag",
            "[VIDEO_DIAG] test={} side=host event=video_send_complete conn={} seq={} result={} send_await_ms={} total_age_ms={} queued_after_send={}",
            self.test_id,
            self.conn_id,
            self.sequence,
            if success { "ok" } else { "error" },
            self.send_started.elapsed().as_millis(),
            self.enqueued_at.elapsed().as_millis(),
            queued_after_send,
        );
    }
}

#[derive(Clone, Copy, Debug)]
struct VideoSendDiag {
    display: i32,
    codec: &'static str,
    keyframe: bool,
    packet_count: usize,
    payload_bytes: u64,
    wire_bytes: u64,
    first_pts: Option<i64>,
    last_pts: Option<i64>,
}

fn extract_video_send_diag(message: &Message) -> Option<VideoSendDiag> {
    let video_frame = match &message.union {
        Some(message::Union::VideoFrame(video_frame)) => video_frame,
        _ => return None,
    };
    use video_frame::Union::*;
    let (codec, encoded_frames): (&'static str, Option<&EncodedVideoFrames>) =
        match &video_frame.union {
            Some(Vp8s(frames)) => ("VP8", Some(frames)),
            Some(Vp9s(frames)) => ("VP9", Some(frames)),
            Some(Av1s(frames)) => ("AV1", Some(frames)),
            Some(H264s(frames)) => ("H264", Some(frames)),
            Some(H265s(frames)) => ("H265", Some(frames)),
            Some(Rgb(_)) => ("RGB", None),
            Some(Yuv(_)) => ("YUV", None),
            Some(_) => ("UNKNOWN", None),
            None => ("NONE", None),
        };
    let packet_count = encoded_frames.map_or(0, |frames| frames.frames.len());
    let payload_bytes = encoded_frames.map_or(0, |frames| {
        frames
            .frames
            .iter()
            .map(|frame| frame.data.len() as u64)
            .sum()
    });
    Some(VideoSendDiag {
        display: video_frame.display,
        codec,
        keyframe: encoded_frames
            .map_or(false, |frames| frames.frames.iter().any(|frame| frame.key)),
        packet_count,
        payload_bytes,
        wire_bytes: message.compute_size(),
        first_pts: encoded_frames.and_then(|frames| frames.frames.first().map(|frame| frame.pts)),
        last_pts: encoded_frames.and_then(|frames| frames.frames.last().map(|frame| frame.pts)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::message_proto::{EncodedVideoFrame, VideoFrame};

    #[test]
    fn extracts_h265_metadata() {
        let mut key_frame = EncodedVideoFrame::new();
        key_frame.data = bytes::Bytes::from_static(b"key");
        key_frame.key = true;
        key_frame.pts = 10;
        let mut delta_frame = EncodedVideoFrame::new();
        delta_frame.data = bytes::Bytes::from_static(b"delta");
        delta_frame.pts = 20;
        let mut encoded_frames = EncodedVideoFrames::new();
        encoded_frames.frames = vec![key_frame, delta_frame];
        let mut video_frame = VideoFrame::new();
        video_frame.display = 2;
        video_frame.set_h265s(encoded_frames);
        let mut message = Message::new();
        message.set_video_frame(video_frame);

        let diag = extract_video_send_diag(&message).expect("video frame diagnostic metadata");

        assert_eq!(diag.display, 2);
        assert_eq!(diag.codec, "H265");
        assert!(diag.keyframe);
        assert_eq!(diag.packet_count, 2);
        assert_eq!(diag.payload_bytes, 8);
        assert_eq!(diag.first_pts, Some(10));
        assert_eq!(diag.last_pts, Some(20));
        assert!(diag.wire_bytes > diag.payload_bytes);
    }
}
