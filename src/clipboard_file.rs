use clipboard::ClipboardFile;
use hbb_common::message_proto::*;

use std::{collections::HashMap, iter::FromIterator, sync::RwLock};

// not actual format id, just a placeholder
const FILEDESCRIPTOR_FORMAT_ID: i32 = 49334;
const FILEDESCRIPTORW_FORMAT_NAME: &str = "FileGroupDescriptorW";
// not actual format id, just a placeholder
const FILECONTENTS_FORMAT_ID: i32 = 49267;
const FILECONTENTS_FORMAT_NAME: &str = "FileContents";

lazy_static::lazy_static! {
    static ref REMOTE_FORMAT_MAP: RwLock<HashMap<i32, String>> = RwLock::new(HashMap::from_iter([
        (
            FILEDESCRIPTOR_FORMAT_ID,
            FILEDESCRIPTORW_FORMAT_NAME.to_string()
        ),
        (FILECONTENTS_FORMAT_ID, FILECONTENTS_FORMAT_NAME.to_string())
    ]));
}

fn get_local_format(remote_id: i32) -> Option<String> {
    REMOTE_FORMAT_MAP
        .read()
        .unwrap()
        .get(&remote_id)
        .map(|s| s.clone())
}

fn add_remote_format(local_name: &str, remote_id: i32) {
    REMOTE_FORMAT_MAP
        .write()
        .unwrap()
        .insert(remote_id, local_name.to_string());
}

pub fn get_file_format_msg() -> Message {
    let fd_format_name = get_local_format(FILEDESCRIPTOR_FORMAT_ID)
        .unwrap_or(FILEDESCRIPTORW_FORMAT_NAME.to_string());
    let fc_format_name =
        get_local_format(FILECONTENTS_FORMAT_ID).unwrap_or(FILECONTENTS_FORMAT_NAME.to_string());
    let format_list = ClipboardFile::FormatList {
        format_list: vec![
            (FILEDESCRIPTOR_FORMAT_ID, fd_format_name),
            (FILECONTENTS_FORMAT_ID, fc_format_name),
        ],
    };
    clip_2_msg(format_list)
}

pub fn clip_2_msg(clip: ClipboardFile) -> Message {
    match clip {
        ClipboardFile::NotifyCallback {
            r#type,
            title,
            text,
        } => Message {
            union: Some(message::Union::MessageBox(MessageBox {
                msgtype: r#type,
                title,
                text,
                link: "".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        },
        ClipboardFile::MonitorReady => Message {
            union: Some(message::Union::Cliprdr(Cliprdr {
                union: Some(cliprdr::Union::Ready(CliprdrMonitorReady {
                    ..Default::default()
                })),
                ..Default::default()
            })),
            ..Default::default()
        },
        ClipboardFile::FormatList { format_list } => {
            let mut formats: Vec<CliprdrFormat> = Vec::new();
            for v in format_list.iter() {
                formats.push(CliprdrFormat {
                    id: v.0,
                    format: v.1.clone(),
                    ..Default::default()
                });
            }
            Message {
                union: Some(message::Union::Cliprdr(Cliprdr {
                    union: Some(cliprdr::Union::FormatList(CliprdrServerFormatList {
                        formats,
                        ..Default::default()
                    })),
                    ..Default::default()
                })),
                ..Default::default()
            }
        }
        ClipboardFile::FormatListResponse { msg_flags } => Message {
            union: Some(message::Union::Cliprdr(Cliprdr {
                union: Some(cliprdr::Union::FormatListResponse(
                    CliprdrServerFormatListResponse {
                        msg_flags,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })),
            ..Default::default()
        },
        ClipboardFile::FormatDataRequest {
            requested_format_id,
        } => Message {
            union: Some(message::Union::Cliprdr(Cliprdr {
                union: Some(cliprdr::Union::FormatDataRequest(
                    CliprdrServerFormatDataRequest {
                        requested_format_id,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })),
            ..Default::default()
        },
        ClipboardFile::FormatDataResponse {
            msg_flags,
            format_data,
        } => Message {
            union: Some(message::Union::Cliprdr(Cliprdr {
                union: Some(cliprdr::Union::FormatDataResponse(
                    CliprdrServerFormatDataResponse {
                        msg_flags,
                        format_data: format_data.into(),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })),
            ..Default::default()
        },
        ClipboardFile::FileContentsRequest {
            stream_id,
            list_index,
            dw_flags,
            n_position_low,
            n_position_high,
            cb_requested,
            have_clip_data_id,
            clip_data_id,
        } => Message {
            union: Some(message::Union::Cliprdr(Cliprdr {
                union: Some(cliprdr::Union::FileContentsRequest(
                    CliprdrFileContentsRequest {
                        stream_id,
                        list_index,
                        dw_flags,
                        n_position_low,
                        n_position_high,
                        cb_requested,
                        have_clip_data_id,
                        clip_data_id,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })),
            ..Default::default()
        },
        ClipboardFile::FileContentsResponse {
            msg_flags,
            stream_id,
            requested_data,
        } => Message {
            union: Some(message::Union::Cliprdr(Cliprdr {
                union: Some(cliprdr::Union::FileContentsResponse(
                    CliprdrFileContentsResponse {
                        msg_flags,
                        stream_id,
                        requested_data: requested_data.into(),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })),
            ..Default::default()
        },
    }
}

pub fn msg_2_clip(msg: Cliprdr) -> Option<ClipboardFile> {
    match msg.union {
        Some(cliprdr::Union::Ready(_)) => Some(ClipboardFile::MonitorReady),
        Some(cliprdr::Union::FormatList(data)) => {
            let mut format_list: Vec<(i32, String)> = Vec::new();
            for v in data.formats.iter() {
                format_list.push((v.id, v.format.clone()));
            }
            Some(ClipboardFile::FormatList { format_list })
        }
        Some(cliprdr::Union::FormatListResponse(data)) => Some(ClipboardFile::FormatListResponse {
            msg_flags: data.msg_flags,
        }),
        Some(cliprdr::Union::FormatDataRequest(data)) => Some(ClipboardFile::FormatDataRequest {
            requested_format_id: data.requested_format_id,
        }),
        Some(cliprdr::Union::FormatDataResponse(data)) => Some(ClipboardFile::FormatDataResponse {
            msg_flags: data.msg_flags,
            format_data: data.format_data.into(),
        }),
        Some(cliprdr::Union::FileContentsRequest(data)) => {
            Some(ClipboardFile::FileContentsRequest {
                stream_id: data.stream_id,
                list_index: data.list_index,
                dw_flags: data.dw_flags,
                n_position_low: data.n_position_low,
                n_position_high: data.n_position_high,
                cb_requested: data.cb_requested,
                have_clip_data_id: data.have_clip_data_id,
                clip_data_id: data.clip_data_id,
            })
        }
        Some(cliprdr::Union::FileContentsResponse(data)) => {
            Some(ClipboardFile::FileContentsResponse {
                msg_flags: data.msg_flags,
                stream_id: data.stream_id,
                requested_data: data.requested_data.into(),
            })
        }
        _ => None,
    }
}
