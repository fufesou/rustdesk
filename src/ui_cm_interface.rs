#[cfg(not(any(target_os = "android", target_os = "ios")))]
use crate::ipc::Connection;
#[cfg(not(any(target_os = "ios")))]
use crate::ipc::{self, Data};
#[cfg(target_os = "windows")]
use crate::{clipboard::ClipboardSide, ipc::ClipboardNonFile};
#[cfg(target_os = "windows")]
use clipboard::ContextSend;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use hbb_common::tokio::sync::mpsc::unbounded_channel;
use hbb_common::{
    allow_err, bail,
    config::{keys::OPTION_ENABLE_FILE_TRANSFER_HASH_VALIDATION, Config},
    fs::{self, get_string, is_write_need_confirmation, new_send_confirm, DigestCheckResult},
    log,
    message_proto::*,
    protobuf::Message as _,
    tokio::{
        self,
        sync::mpsc::{self, UnboundedSender},
        task::spawn_blocking,
    },
    ResultType,
};
#[cfg(target_os = "windows")]
use hbb_common::{
    config::{keys::*, option2bool},
    tokio::sync::Mutex as TokioMutex,
};
use serde_derive::Serialize;
#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
use std::iter::FromIterator;
#[cfg(target_os = "windows")]
use std::sync::Arc;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    path::Path,
    sync::{
        atomic::{AtomicI64, Ordering},
        RwLock,
    },
};

/// Maximum number of files allowed in a single validate_read_access request.
/// This prevents excessive I/O and potential UI freezes when dealing with large directories.
#[cfg(not(any(target_os = "ios")))]
const MAX_VALIDATED_FILES: usize = 10_000;

#[derive(Serialize, Clone)]
pub struct Client {
    pub id: i32,
    pub authorized: bool,
    pub disconnected: bool,
    pub is_file_transfer: bool,
    pub is_view_camera: bool,
    pub is_terminal: bool,
    pub port_forward: String,
    pub name: String,
    pub peer_id: String,
    pub keyboard: bool,
    pub clipboard: bool,
    pub audio: bool,
    pub file: bool,
    pub restart: bool,
    pub recording: bool,
    pub block_input: bool,
    pub from_switch: bool,
    pub in_voice_call: bool,
    pub incoming_voice_call: bool,
    #[serde(skip)]
    #[cfg(not(any(target_os = "ios")))]
    tx: UnboundedSender<Data>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct IpcTaskRunner<T: InvokeUiCM> {
    stream: Connection,
    cm: ConnectionManager<T>,
    tx: mpsc::UnboundedSender<Data>,
    rx: mpsc::UnboundedReceiver<Data>,
    close: bool,
    running: bool,
    conn_id: i32,
    #[cfg(target_os = "windows")]
    file_transfer_enabled: bool,
    #[cfg(target_os = "windows")]
    file_transfer_enabled_peer: bool,
}

lazy_static::lazy_static! {
    static ref CLIENTS: RwLock<HashMap<i32, Client>> = Default::default();
}

static CLICK_TIME: AtomicI64 = AtomicI64::new(0);

#[derive(Clone)]
pub struct ConnectionManager<T: InvokeUiCM> {
    pub ui_handler: T,
}

pub trait InvokeUiCM: Send + Clone + 'static + Sized {
    fn add_connection(&self, client: &Client);

    fn remove_connection(&self, id: i32, close: bool);

    fn new_message(&self, id: i32, text: String);

    fn change_theme(&self, dark: String);

    fn change_language(&self);

    fn show_elevation(&self, show: bool);

    fn update_voice_call_state(&self, client: &Client);

    fn file_transfer_log(&self, action: &str, log: &str);
}

impl<T: InvokeUiCM> Deref for ConnectionManager<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.ui_handler
    }
}

impl<T: InvokeUiCM> DerefMut for ConnectionManager<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ui_handler
    }
}

impl<T: InvokeUiCM> ConnectionManager<T> {
    fn add_connection(
        &self,
        id: i32,
        is_file_transfer: bool,
        is_view_camera: bool,
        is_terminal: bool,
        port_forward: String,
        peer_id: String,
        name: String,
        authorized: bool,
        keyboard: bool,
        clipboard: bool,
        audio: bool,
        file: bool,
        restart: bool,
        recording: bool,
        block_input: bool,
        from_switch: bool,
        #[cfg(not(any(target_os = "ios")))] tx: mpsc::UnboundedSender<Data>,
    ) {
        let client = Client {
            id,
            authorized,
            disconnected: false,
            is_file_transfer,
            is_view_camera,
            is_terminal,
            port_forward,
            name: name.clone(),
            peer_id: peer_id.clone(),
            keyboard,
            clipboard,
            audio,
            file,
            restart,
            recording,
            block_input,
            from_switch,
            #[cfg(not(any(target_os = "ios")))]
            tx,
            in_voice_call: false,
            incoming_voice_call: false,
        };
        CLIENTS
            .write()
            .unwrap()
            .retain(|_, c| !(c.disconnected && c.peer_id == client.peer_id));
        CLIENTS.write().unwrap().insert(id, client.clone());
        self.ui_handler.add_connection(&client);
    }

    #[inline]
    #[cfg(target_os = "windows")]
    fn is_authorized(&self, id: i32) -> bool {
        CLIENTS
            .read()
            .unwrap()
            .get(&id)
            .map(|c| c.authorized)
            .unwrap_or(false)
    }

    fn remove_connection(&self, id: i32, close: bool) {
        if close {
            CLIENTS.write().unwrap().remove(&id);
        } else {
            CLIENTS
                .write()
                .unwrap()
                .get_mut(&id)
                .map(|c| c.disconnected = true);
        }

        #[cfg(target_os = "windows")]
        {
            crate::clipboard::try_empty_clipboard_files(ClipboardSide::Host, id);
        }

        #[cfg(any(target_os = "android"))]
        if CLIENTS
            .read()
            .unwrap()
            .iter()
            .filter(|(_k, v)| !v.is_file_transfer && !v.is_terminal)
            .next()
            .is_none()
        {
            if let Err(e) =
                scrap::android::call_main_service_set_by_name("stop_capture", None, None)
            {
                log::debug!("stop_capture err:{}", e);
            }
        }

        self.ui_handler.remove_connection(id, close);
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    fn show_elevation(&self, show: bool) {
        self.ui_handler.show_elevation(show);
    }

    #[cfg(not(target_os = "ios"))]
    fn voice_call_started(&self, id: i32) {
        if let Some(client) = CLIENTS.write().unwrap().get_mut(&id) {
            client.incoming_voice_call = false;
            client.in_voice_call = true;
            self.ui_handler.update_voice_call_state(client);
        }
    }

    #[cfg(not(target_os = "ios"))]
    fn voice_call_incoming(&self, id: i32) {
        if let Some(client) = CLIENTS.write().unwrap().get_mut(&id) {
            client.incoming_voice_call = true;
            client.in_voice_call = false;
            self.ui_handler.update_voice_call_state(client);
        }
    }

    #[cfg(not(target_os = "ios"))]
    fn voice_call_closed(&self, id: i32, _reason: &str) {
        if let Some(client) = CLIENTS.write().unwrap().get_mut(&id) {
            client.incoming_voice_call = false;
            client.in_voice_call = false;
            self.ui_handler.update_voice_call_state(client);
        }
    }
}

#[inline]
#[cfg(not(any(target_os = "ios")))]
pub fn check_click_time(id: i32) {
    if let Some(client) = CLIENTS.read().unwrap().get(&id) {
        allow_err!(client.tx.send(Data::ClickTime(0)));
    };
}

#[inline]
pub fn get_click_time() -> i64 {
    CLICK_TIME.load(Ordering::SeqCst)
}

#[inline]
#[cfg(not(any(target_os = "ios")))]
pub fn authorize(id: i32) {
    if let Some(client) = CLIENTS.write().unwrap().get_mut(&id) {
        client.authorized = true;
        allow_err!(client.tx.send(Data::Authorize));
    };
}

#[inline]
#[cfg(not(any(target_os = "ios")))]
pub fn close(id: i32) {
    if let Some(client) = CLIENTS.read().unwrap().get(&id) {
        allow_err!(client.tx.send(Data::Close));
    };
}

#[inline]
pub fn remove(id: i32) {
    CLIENTS.write().unwrap().remove(&id);
}

// server mode send chat to peer
#[inline]
#[cfg(not(any(target_os = "ios")))]
pub fn send_chat(id: i32, text: String) {
    let clients = CLIENTS.read().unwrap();
    if let Some(client) = clients.get(&id) {
        allow_err!(client.tx.send(Data::ChatMessage { text }));
    }
}

#[inline]
#[cfg(not(any(target_os = "ios")))]
pub fn switch_permission(id: i32, name: String, enabled: bool) {
    if let Some(client) = CLIENTS.read().unwrap().get(&id) {
        allow_err!(client.tx.send(Data::SwitchPermission { name, enabled }));
    };
}

#[inline]
#[cfg(target_os = "android")]
pub fn switch_permission_all(name: String, enabled: bool) {
    for (_, client) in CLIENTS.read().unwrap().iter() {
        allow_err!(client.tx.send(Data::SwitchPermission {
            name: name.clone(),
            enabled
        }));
    }
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn get_clients_state() -> String {
    let clients = CLIENTS.read().unwrap();
    let res = Vec::from_iter(clients.values().cloned());
    serde_json::to_string(&res).unwrap_or("".into())
}

#[inline]
pub fn get_clients_length() -> usize {
    let clients = CLIENTS.read().unwrap();
    clients.len()
}

#[inline]
#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "ios")))]
pub fn switch_back(id: i32) {
    if let Some(client) = CLIENTS.read().unwrap().get(&id) {
        allow_err!(client.tx.send(Data::SwitchSidesBack));
    };
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl<T: InvokeUiCM> IpcTaskRunner<T> {
    async fn run(&mut self) {
        use hbb_common::config::LocalConfig;

        // for tmp use, without real conn id
        let mut write_jobs: Vec<fs::TransferJob> = Vec::new();

        #[cfg(target_os = "windows")]
        let is_authorized = self.cm.is_authorized(self.conn_id);

        #[cfg(target_os = "windows")]
        let rx_clip_holder;
        let mut rx_clip;
        let _tx_clip;
        #[cfg(target_os = "windows")]
        if self.conn_id > 0 && is_authorized {
            log::debug!("Clipboard is enabled from client peer: type 1");
            let conn_id = self.conn_id;
            rx_clip_holder = (
                clipboard::get_rx_cliprdr_server(conn_id),
                Some(crate::SimpleCallOnReturn {
                    b: true,
                    f: Box::new(move || {
                        clipboard::remove_channel_by_conn_id(conn_id);
                    }),
                }),
            );
            rx_clip = rx_clip_holder.0.lock().await;
        } else {
            log::debug!("Clipboard is enabled from client peer, actually useless: type 2");
            let rx_clip2;
            (_tx_clip, rx_clip2) = unbounded_channel::<clipboard::ClipboardFile>();
            rx_clip_holder = (Arc::new(TokioMutex::new(rx_clip2)), None);
            rx_clip = rx_clip_holder.0.lock().await;
        }
        #[cfg(not(target_os = "windows"))]
        {
            (_tx_clip, rx_clip) = unbounded_channel::<i32>();
        }

        #[cfg(target_os = "windows")]
        {
            if ContextSend::is_enabled() {
                log::debug!("Clipboard is enabled");
                allow_err!(
                    self.stream
                        .send(&Data::ClipboardFile(clipboard::ClipboardFile::MonitorReady))
                        .await
                );
            }
        }
        let (tx_log, mut rx_log) = mpsc::unbounded_channel::<String>();

        self.running = false;
        loop {
            tokio::select! {
                res = self.stream.next() => {
                    match res {
                        Err(err) => {
                            log::info!("cm ipc connection closed: {}", err);
                            break;
                        }
                        Ok(Some(data)) => {
                            match data {
                                Data::Login{id, is_file_transfer, is_view_camera, is_terminal, port_forward, peer_id, name, authorized, keyboard, clipboard, audio, file, file_transfer_enabled: _file_transfer_enabled, restart, recording, block_input, from_switch} => {
                                    log::debug!("conn_id: {}", id);
                                    self.cm.add_connection(id, is_file_transfer, is_view_camera, is_terminal, port_forward, peer_id, name, authorized, keyboard, clipboard, audio, file, restart, recording, block_input, from_switch, self.tx.clone());
                                    self.conn_id = id;
                                    #[cfg(target_os = "windows")]
                                    {
                                        self.file_transfer_enabled = _file_transfer_enabled;
                                    }
                                    self.running = true;
                                    break;
                                }
                                Data::Close => {
                                    log::info!("cm ipc connection closed from connection request");
                                    break;
                                }
                                Data::Disconnected => {
                                    self.close = false;
                                    log::info!("cm ipc connection disconnect");
                                    break;
                                }
                                Data::PrivacyModeState((_id, _, _)) => {
                                    #[cfg(windows)]
                                    cm_inner_send(_id, data);
                                }
                                Data::ClickTime(ms) => {
                                    CLICK_TIME.store(ms, Ordering::SeqCst);
                                }
                                Data::ChatMessage { text } => {
                                    self.cm.new_message(self.conn_id, text);
                                }
                                Data::FS(mut fs) => {
                                    if let ipc::FS::WriteBlock { id, file_num, data: _, compressed } = fs {
                                        if let Ok(bytes) = self.stream.next_raw().await {
                                            fs = ipc::FS::WriteBlock{id, file_num, data:bytes.into(), compressed};
                                            handle_fs(fs, &mut write_jobs, &self.tx, Some(&tx_log)).await;
                                        }
                                    } else {
                                        handle_fs(fs, &mut write_jobs, &self.tx, Some(&tx_log)).await;
                                    }
                                    let log = fs::serialize_transfer_jobs(&write_jobs);
                                    self.cm.ui_handler.file_transfer_log("transfer", &log);
                                }
                                Data::FileTransferLog((action, log)) => {
                                    self.cm.ui_handler.file_transfer_log(&action, &log);
                                }
                                #[cfg(target_os = "windows")]
                                Data::ClipboardFile(_clip) => {
                                    let is_stopping_allowed = _clip.is_beginning_message();
                                    let is_clipboard_enabled = ContextSend::is_enabled();
                                    let file_transfer_enabled = self.file_transfer_enabled;
                                    let stop = !is_stopping_allowed && !(is_clipboard_enabled && file_transfer_enabled);
                                    log::debug!(
                                        "Process clipboard message from client peer, stop: {}, is_stopping_allowed: {}, is_clipboard_enabled: {}, file_transfer_enabled: {}",
                                        stop, is_stopping_allowed, is_clipboard_enabled, file_transfer_enabled);
                                    if stop {
                                        ContextSend::set_is_stopped();
                                    } else {
                                        if !is_authorized {
                                            log::debug!("Clipboard message from client peer, but not authorized");
                                            continue;
                                        }
                                        let conn_id = self.conn_id;
                                        let _ = ContextSend::proc(|context| -> ResultType<()> {
                                            context.server_clip_file(conn_id, _clip)
                                                .map_err(|e| e.into())
                                        });
                                    }
                                }
                                Data::ClipboardFileEnabled(_enabled) => {
                                    #[cfg(target_os = "windows")]
                                    {
                                        self.file_transfer_enabled_peer = _enabled;
                                    }
                                }
                                Data::Theme(dark) => {
                                    self.cm.change_theme(dark);
                                }
                                Data::Language(lang) => {
                                    LocalConfig::set_option("lang".to_owned(), lang);
                                    self.cm.change_language();
                                }
                                Data::DataPortableService(ipc::DataPortableService::CmShowElevation(show)) => {
                                    self.cm.show_elevation(show);
                                }
                                Data::StartVoiceCall => {
                                    self.cm.voice_call_started(self.conn_id);
                                }
                                Data::VoiceCallIncoming => {
                                    self.cm.voice_call_incoming(self.conn_id);
                                }
                                Data::CloseVoiceCall(reason) => {
                                    self.cm.voice_call_closed(self.conn_id, reason.as_str());
                                }
                                #[cfg(target_os = "windows")]
                                Data::ClipboardNonFile(_) => {
                                    match crate::clipboard::check_clipboard_cm() {
                                        Ok(multi_clipoards) => {
                                            let mut raw_contents = bytes::BytesMut::new();
                                            let mut main_data = vec![];
                                            for c in multi_clipoards.clipboards.into_iter() {
                                                let content_len = c.content.len();
                                                let (content, next_raw) = {
                                                    // TODO: find out a better threshold
                                                    if content_len > 1024 * 3 {
                                                        raw_contents.extend(c.content);
                                                        (bytes::Bytes::new(), true)
                                                    } else {
                                                        (c.content, false)
                                                    }
                                                };
                                                main_data.push(ClipboardNonFile {
                                                    compress: c.compress,
                                                    content,
                                                    content_len,
                                                    next_raw,
                                                    width: c.width,
                                                    height: c.height,
                                                    format: c.format.value(),
                                                    special_name: c.special_name,
                                                });
                                            }
                                            allow_err!(self.stream.send(&Data::ClipboardNonFile(Some(("".to_owned(), main_data)))).await);
                                            if !raw_contents.is_empty() {
                                                allow_err!(self.stream.send_raw(raw_contents.into()).await);
                                            }
                                        }
                                        Err(e) => {
                                            log::debug!("Failed to get clipboard content. {}", e);
                                            allow_err!(self.stream.send(&Data::ClipboardNonFile(Some((format!("{}", e), vec![])))).await);
                                        }
                                    }
                                }
                                _ => {

                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some(data) = self.rx.recv() => {
                    if let Err(e) = self.stream.send(&data).await {
                        log::error!("error encountered in IPC task, quitting: {}", e);
                        break;
                    }
                    match &data {
                        Data::SwitchPermission{name: _name, enabled: _enabled} => {
                            #[cfg(target_os = "windows")]
                            if _name == "file" {
                                self.file_transfer_enabled = *_enabled;
                            }
                        }
                        Data::Authorize => {
                            self.running = true;
                            break;
                        }
                        _ => {
                        }
                    }
                },
                clip_file = rx_clip.recv() => match clip_file {
                    Some(_clip) => {
                        #[cfg(target_os = "windows")]
                        {
                            let is_stopping_allowed = _clip.is_stopping_allowed();
                            let is_clipboard_enabled = ContextSend::is_enabled();
                            let file_transfer_enabled = self.file_transfer_enabled;
                            let file_transfer_enabled_peer = self.file_transfer_enabled_peer;
                            let stop = is_stopping_allowed && !(is_clipboard_enabled && file_transfer_enabled && file_transfer_enabled_peer);
                            log::debug!(
                                "Process clipboard message from clip, stop: {}, is_stopping_allowed: {}, is_clipboard_enabled: {}, file_transfer_enabled: {}, file_transfer_enabled_peer: {}",
                                stop, is_stopping_allowed, is_clipboard_enabled, file_transfer_enabled, file_transfer_enabled_peer);
                            if stop {
                                ContextSend::set_is_stopped();
                            } else {
                                if _clip.is_beginning_message() && crate::get_builtin_option(OPTION_ONE_WAY_FILE_TRANSFER) == "Y" {
                                    // If one way file transfer is enabled, don't send clipboard file to client
                                    // Don't call `ContextSend::set_is_stopped()`, because it will stop bidirectional file copy&paste.
                                } else {
                                    allow_err!(self.tx.send(Data::ClipboardFile(_clip)));
                                }
                            }
                        }
                    }
                    None => {
                        //
                    }
                },
                Some(job_log) = rx_log.recv() => {
                    self.cm.ui_handler.file_transfer_log("transfer", &job_log);
                }
            }
        }
    }

    async fn ipc_task(stream: Connection, cm: ConnectionManager<T>) {
        log::debug!("ipc task begin");
        let (tx, rx) = mpsc::unbounded_channel::<Data>();
        let mut task_runner = Self {
            stream,
            cm,
            tx,
            rx,
            close: true,
            running: true,
            conn_id: 0,
            #[cfg(target_os = "windows")]
            file_transfer_enabled: false,
            #[cfg(target_os = "windows")]
            file_transfer_enabled_peer: false,
        };

        while task_runner.running {
            task_runner.run().await;
        }
        if task_runner.conn_id > 0 {
            task_runner
                .cm
                .remove_connection(task_runner.conn_id, task_runner.close);
        }
        log::debug!("ipc task end");
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tokio::main(flavor = "current_thread")]
pub async fn start_ipc<T: InvokeUiCM>(cm: ConnectionManager<T>) {
    #[cfg(target_os = "windows")]
    ContextSend::enable(option2bool(
        OPTION_ENABLE_FILE_TRANSFER,
        &Config::get_option(OPTION_ENABLE_FILE_TRANSFER),
    ));
    match ipc::new_listener("_cm").await {
        Ok(mut incoming) => {
            while let Some(result) = incoming.next().await {
                match result {
                    Ok(stream) => {
                        log::debug!("Got new connection");
                        tokio::spawn(IpcTaskRunner::<T>::ipc_task(
                            Connection::new(stream),
                            cm.clone(),
                        ));
                    }
                    Err(err) => {
                        log::error!("Couldn't get cm client: {:?}", err);
                    }
                }
            }
        }
        Err(err) => {
            log::error!("Failed to start cm ipc server: {}", err);
        }
    }
    quit_cm();
}

#[cfg(target_os = "android")]
#[tokio::main(flavor = "current_thread")]
pub async fn start_listen<T: InvokeUiCM>(
    cm: ConnectionManager<T>,
    mut rx: mpsc::UnboundedReceiver<Data>,
    tx: mpsc::UnboundedSender<Data>,
) {
    let mut current_id = 0;
    let mut write_jobs: Vec<fs::TransferJob> = Vec::new();
    loop {
        match rx.recv().await {
            Some(Data::Login {
                id,
                is_file_transfer,
                is_view_camera,
                is_terminal,
                port_forward,
                peer_id,
                name,
                authorized,
                keyboard,
                clipboard,
                audio,
                file,
                restart,
                recording,
                block_input,
                from_switch,
                ..
            }) => {
                current_id = id;
                cm.add_connection(
                    id,
                    is_file_transfer,
                    is_view_camera,
                    is_terminal,
                    port_forward,
                    peer_id,
                    name,
                    authorized,
                    keyboard,
                    clipboard,
                    audio,
                    file,
                    restart,
                    recording,
                    block_input,
                    from_switch,
                    tx.clone(),
                );
            }
            Some(Data::ChatMessage { text }) => {
                cm.new_message(current_id, text);
            }
            Some(Data::FS(fs)) => {
                handle_fs(fs, &mut write_jobs, &tx, None).await;
            }
            Some(Data::Close) => {
                break;
            }
            Some(Data::StartVoiceCall) => {
                cm.voice_call_started(current_id);
            }
            Some(Data::VoiceCallIncoming) => {
                cm.voice_call_incoming(current_id);
            }
            Some(Data::CloseVoiceCall(reason)) => {
                cm.voice_call_closed(current_id, reason.as_str());
            }
            None => {
                break;
            }
            _ => {}
        }
    }
    cm.remove_connection(current_id, true);
}

#[cfg(not(any(target_os = "ios")))]
async fn handle_fs(
    fs: ipc::FS,
    write_jobs: &mut Vec<fs::TransferJob>,
    tx: &UnboundedSender<Data>,
    tx_log: Option<&UnboundedSender<String>>,
) {
    use std::path::PathBuf;

    use hbb_common::fs::serialize_transfer_job;

    match fs {
        ipc::FS::ReadEmptyDirs {
            dir,
            include_hidden,
        } => {
            read_empty_dirs(&dir, include_hidden, tx).await;
        }
        ipc::FS::ReadDir {
            dir,
            include_hidden,
        } => {
            read_dir(&dir, include_hidden, tx).await;
        }
        ipc::FS::RemoveDir {
            path,
            id,
            recursive,
        } => {
            remove_dir(path, id, recursive, tx).await;
        }
        ipc::FS::RemoveFile { path, id, file_num } => {
            remove_file(path, id, file_num, tx).await;
        }
        ipc::FS::CreateDir { path, id } => {
            create_dir(path, id, tx).await;
        }
        ipc::FS::NewWrite {
            path,
            id,
            file_num,
            mut files,
            overwrite_detection,
            total_size,
            conn_id,
        } => {
            // Validate file names to prevent path traversal attacks.
            // This must be done BEFORE any path operations to ensure attackers cannot
            // escape the target directory using names like "../../malicious.txt"
            if let Err(e) = validate_transfer_file_names(&files) {
                log::warn!("Path traversal attempt detected for {}: {}", path, e);
                send_raw(fs::new_error(id, e, file_num), tx);
                return;
            }

            // Check parent directory access before allowing write
            // For new files, the file itself doesn't exist yet, so we canonicalize the parent directory
            let path_obj = Path::new(&path);
            if let Err(e) = validate_parent_and_canonicalize(&path_obj) {
                log::warn!("Write access denied for {}: {}", path, e);
                send_raw(fs::new_error(id, e, file_num), tx);
                return;
            }

            // Convert files to FileEntry and validate write paths
            let file_entries: Vec<FileEntry> = files
                .drain(..)
                .map(|f| FileEntry {
                    name: f.0,
                    modified_time: f.1,
                    ..Default::default()
                })
                .collect();

            // Validate that all intermediate directories for each file are accessible
            if let Err(e) = validate_write_paths(&path_obj, &file_entries) {
                log::warn!("Write path validation failed for {}: {}", path, e);
                send_raw(fs::new_error(id, e, file_num), tx);
                return;
            }

            // cm has no show_hidden context
            // dummy remote, show_hidden, is_remote
            let mut job = fs::TransferJob::new_write(
                id,
                fs::JobType::Generic,
                "".to_string(),
                fs::DataSource::FilePath(PathBuf::from(&path)),
                file_num,
                false,
                false,
                file_entries,
                overwrite_detection,
            );
            job.total_size = total_size;
            job.conn_id = conn_id;
            write_jobs.push(job);
        }
        ipc::FS::CancelWrite { id } => {
            if let Some(job) = fs::remove_job(id, write_jobs) {
                job.remove_download_file();
                tx_log.map(|tx: &UnboundedSender<String>| {
                    tx.send(serialize_transfer_job(&job, false, true, ""))
                });
            }
        }
        ipc::FS::WriteDone { id, file_num } => {
            if let Some(job) = fs::remove_job(id, write_jobs) {
                job.modify_time();
                send_raw(fs::new_done(id, file_num), tx);
                tx_log.map(|tx| tx.send(serialize_transfer_job(&job, true, false, "")));
            }
        }
        ipc::FS::WriteError { id, file_num, err } => {
            if let Some(job) = fs::remove_job(id, write_jobs) {
                tx_log.map(|tx| tx.send(serialize_transfer_job(&job, false, false, &err)));
                send_raw(fs::new_error(job.id(), err, file_num), tx);
            }
        }
        ipc::FS::WriteBlock {
            id,
            file_num,
            data,
            compressed,
        } => {
            if let Some(job) = fs::get_job(id, write_jobs) {
                if let Err(err) = job
                    .write(FileTransferBlock {
                        id,
                        file_num,
                        data,
                        compressed,
                        ..Default::default()
                    })
                    .await
                {
                    send_raw(fs::new_error(id, err, file_num), &tx);
                }
            }
        }
        ipc::FS::CheckDigest {
            id,
            file_num,
            file_size,
            last_modified,
            is_upload,
            is_resume,
        } => {
            if let Some(job) = fs::get_job(id, write_jobs) {
                let mut req = FileTransferSendConfirmRequest {
                    id,
                    file_num,
                    union: Some(file_transfer_send_confirm_request::Union::OffsetBlk(0)),
                    ..Default::default()
                };
                let digest = FileTransferDigest {
                    id,
                    file_num,
                    last_modified,
                    file_size,
                    ..Default::default()
                };
                if let Some(file) = job.files().get(file_num as usize) {
                    if let fs::DataSource::FilePath(p) = &job.data_source {
                        let path = get_string(&fs::TransferJob::join(p, &file.name));
                        match is_write_need_confirmation(is_resume, &path, &digest) {
                            Ok(digest_result) => {
                                job.set_digest(file_size, last_modified);
                                match digest_result {
                                    DigestCheckResult::IsSame => {
                                        req.set_skip(true);
                                        let msg_out = new_send_confirm(req);
                                        send_raw(msg_out, &tx);
                                    }
                                    DigestCheckResult::NeedConfirm(mut digest) => {
                                        // upload to server, but server has the same file, request
                                        digest.is_upload = is_upload;
                                        let mut msg_out = Message::new();
                                        let mut fr = FileResponse::new();
                                        fr.set_digest(digest);
                                        msg_out.set_file_response(fr);
                                        send_raw(msg_out, &tx);
                                    }
                                    DigestCheckResult::NoSuchFile => {
                                        let msg_out = new_send_confirm(req);
                                        send_raw(msg_out, &tx);
                                    }
                                }
                            }
                            Err(err) => {
                                send_raw(fs::new_error(id, err, file_num), &tx);
                            }
                        }
                    }
                }
            }
        }
        ipc::FS::SendConfirm(bytes) => {
            if let Ok(r) = FileTransferSendConfirmRequest::parse_from_bytes(&bytes) {
                if let Some(job) = fs::get_job(r.id, write_jobs) {
                    job.confirm(&r).await;
                }
            }
        }
        ipc::FS::Rename { id, path, new_name } => {
            rename_file(path, new_name, id, tx).await;
        }
        ipc::FS::ValidateReadAccess {
            path,
            id,
            include_hidden,
            conn_id,
        } => {
            validate_read_access(path, include_hidden, id, conn_id, tx).await;
        }
        ipc::FS::ReadAllFiles {
            path,
            id,
            include_hidden,
            conn_id,
        } => {
            read_all_files(path, include_hidden, id, conn_id, tx).await;
        }
        _ => {}
    }
}

fn compute_hash(path: &Path) -> ResultType<Option<String>> {
    if !Config::get_bool_option(OPTION_ENABLE_FILE_TRANSFER_HASH_VALIDATION) {
        // Verify read access; close immediately.
        let _ = std::fs::File::open(path)?;
        return Ok(None);
    }
    let mut file = std::fs::File::open(&path)?;
    fs::compute_file_hash_sync(&mut file, Some(fs::MAX_HASH_BYTES))
}

// Although the following function is typically only needed on Windows,
// we include it for other platforms (except iOS) as well to maintain consistency,
// and it does not add significant overhead.
//
/// Validates that all parent directories of the given path are accessible (readable).
/// This prevents bypassing directory-level access restrictions by directly accessing
/// child files/directories using their full paths.
///
/// On Windows, denying access to a directory only prevents listing its contents,
/// but child items can still be accessed if their full paths are known.
/// This function ensures that if any parent directory is inaccessible,
/// the entire path is considered inaccessible.
#[cfg(not(any(target_os = "ios")))]
fn check_parent_directories_access(path: &Path) -> ResultType<()> {
    for ancestor in path.ancestors().skip(1) {
        // Skip empty paths
        if ancestor.as_os_str().is_empty() {
            continue;
        }

        // Check directory list permission for all ancestors including root.
        // While canonicalize() validates path reachability (Traverse permission),
        // we also enforce List permission to prevent accessing files when parent
        // directories are intentionally hidden. This implements defense-in-depth:
        // even if root directory list access is rarely restricted, checking it
        // ensures consistent security policy across all path levels.
        if ancestor.is_dir() {
            if let Err(e) = std::fs::read_dir(ancestor) {
                log::error!(
                    "access denied to parent directory '{}': {}",
                    ancestor.display(),
                    e
                );
                bail!("access denied: insufficient permissions to access path");
            }
        }
    }
    Ok(())
}

/// Validates and canonicalizes a path, checking parent directory access.
/// This is the main helper function to ensure a path is accessible before operations.
#[inline]
#[cfg(not(any(target_os = "ios")))]
fn validate_and_canonicalize(path: &Path) -> ResultType<std::path::PathBuf> {
    let canonical = path.canonicalize()?;
    check_parent_directories_access(&canonical)?;
    Ok(canonical)
}

/// Validates parent directory access and canonicalizes the parent path.
/// Used for operations like create_dir where the target itself doesn't exist yet.
#[inline]
#[cfg(not(any(target_os = "ios")))]
fn validate_parent_and_canonicalize(path: &Path) -> ResultType<std::path::PathBuf> {
    let parent = match path.parent() {
        Some(p) => p,
        None => {
            bail!("invalid path: no parent directory");
        }
    };
    let canonical = parent.canonicalize()?;
    check_parent_directories_access(&canonical)?;
    Ok(canonical)
}

/// Validates that a file name does not contain path traversal sequences.
/// This prevents attackers from escaping the base directory by using names like
/// "../../../etc/passwd" or "..\\..\\Windows\\System32\\malicious.dll".
#[cfg(not(any(target_os = "ios")))]
fn validate_file_name_no_traversal(name: &str) -> ResultType<()> {
    // Check for path traversal patterns
    // We check for both Unix and Windows path separators
    let components: Vec<&str> = name
        .split(|c| c == '/' || c == '\\')
        .filter(|s| !s.is_empty())
        .collect();

    for component in &components {
        if *component == ".." {
            bail!("path traversal detected in file name");
        }
    }

    // On Windows, also check for drive letters (e.g., "C:")
    #[cfg(windows)]
    {
        if name.len() >= 2 {
            let bytes = name.as_bytes();
            if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                bail!("absolute path detected in file name");
            }
        }
    }

    // Check for names starting with path separator (absolute paths on Unix)
    if name.starts_with('/') || name.starts_with('\\') {
        bail!("absolute path detected in file name");
    }

    Ok(())
}

/// Validates all file names in a transfer request to prevent path traversal attacks.
/// Returns an error if any file name contains dangerous path components.
#[cfg(not(any(target_os = "ios")))]
fn validate_transfer_file_names(files: &[(String, u64)]) -> ResultType<()> {
    for (name, _) in files {
        validate_file_name_no_traversal(name)?;
    }
    Ok(())
}

/// Validates that all files in a write operation have accessible parent directories.
/// This prevents writing files to directories where intermediate paths are restricted.
/// For example, if base="C:\a\b" and file="c\d\e.txt", this validates
/// that the user has access to both "C:\a\b\c" and "C:\a\b\c\d".
#[cfg(not(any(target_os = "ios")))]
fn validate_write_paths(base_path: &Path, files: &[FileEntry]) -> ResultType<()> {
    let canonical_base = match base_path.canonicalize() {
        Ok(p) => p,
        Err(e) => bail!("cannot resolve base path: {}", e),
    };

    for file in files {
        if file.name.is_empty() {
            continue;
        }

        let full_path = canonical_base.join(&file.name);

        // Validate the parent directory of each file
        if let Some(parent) = full_path.parent() {
            // The parent might not exist yet (we'll create it), so check its parent
            match validate_parent_and_canonicalize(parent) {
                Ok(_) => {}
                Err(e) => {
                    bail!("access denied for file '{}': {}", file.name, e);
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "ios")))]
async fn validate_read_access(
    path: String,
    include_hidden: bool,
    id: i32,
    conn_id: i32,
    tx: &UnboundedSender<Data>,
) {
    let result = spawn_blocking(move || {
        let path_obj = Path::new(&path);

        match validate_and_canonicalize(&path_obj) {
            Ok(canonical_path) => {
                let canonical_str = canonical_path.to_string_lossy().to_string();

                if canonical_path.is_file() {
                    match std::fs::metadata(&canonical_path) {
                        Ok(meta) => {
                            let size = meta.len();
                            let modified_time = meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs())
                                .unwrap_or(0);

                            let hash = compute_hash(&canonical_path)?;

                            let file_entry = ipc::ValidatedFile {
                                name: String::new(),
                                size,
                                modified_time,
                                hash,
                            };
                            Ok((canonical_str, vec![file_entry]))
                        }
                        Err(e) => bail!("stat file failed: {}", e),
                    }
                } else if canonical_path.is_dir() {
                    match fs::get_recursive_files(&canonical_str, include_hidden) {
                        Ok(files) => {
                            // Check file count limit to prevent excessive I/O
                            if files.len() > MAX_VALIDATED_FILES {
                                log::warn!(
                                    "Too many files in directory for validation: {} > {}",
                                    files.len(),
                                    MAX_VALIDATED_FILES
                                );
                                bail!(
                                    "too many files in directory ({} > {})",
                                    files.len(),
                                    MAX_VALIDATED_FILES
                                );
                            }

                            let mut validated_files = Vec::with_capacity(files.len());
                            for f in files {
                                let full_path = canonical_path.join(&f.name);

                                // Validate parent directory access for files in subdirectories
                                if f.name.contains('/') || f.name.contains('\\') {
                                    if let Some(parent) = full_path.parent() {
                                        if let Err(e) = validate_and_canonicalize(parent) {
                                            bail!("access denied to parent of '{}': {}", f.name, e);
                                        }
                                    }
                                }

                                // Check file accessibility before computing hash
                                if let Err(e) = std::fs::metadata(&full_path) {
                                    bail!("stat file failed for {}: {}", f.name, e);
                                }
                                let hash = compute_hash(&full_path)?;
                                validated_files.push(ipc::ValidatedFile {
                                    name: f.name,
                                    size: f.size,
                                    modified_time: f.modified_time,
                                    hash,
                                });
                            }
                            Ok((canonical_str, validated_files))
                        }
                        Err(e) => bail!("list directory failed: {}", e),
                    }
                } else {
                    bail!("path is neither file nor directory: {}", canonical_str)
                }
            }
            Err(e) => bail!("canonicalize failed: {}", e),
        }
    })
    .await;

    let result = match result {
        Ok(Ok((path, files))) => Ok((path, files)),
        Ok(Err(e)) => Err(format!("validation failed: {}", e)),
        Err(e) => Err(format!("validation task failed: {}", e)),
    };

    let _ = tx.send(Data::ReadAccessValidated {
        id,
        conn_id,
        result,
    });
}

#[cfg(not(any(target_os = "ios")))]
async fn read_all_files(
    path: String,
    include_hidden: bool,
    id: i32,
    conn_id: i32,
    tx: &UnboundedSender<Data>,
) {
    let path_clone = path.clone();
    let result = spawn_blocking(move || {
        let path_obj = Path::new(&path);

        // Canonicalize the path so that both ACL checks and recursive listing
        // operate on the same, fully-resolved filesystem path.
        match validate_and_canonicalize(&path_obj) {
            Ok(canonical_path) => {
                let canonical_str = canonical_path.to_string_lossy().to_string();
                fs::get_recursive_files(&canonical_str, include_hidden)
            }
            Err(e) => bail!("canonicalize failed: {}", e),
        }
    })
    .await;

    let result = match result {
        Ok(Ok(files)) => {
            // Serialize FileDirectory to protobuf bytes
            let mut fd = FileDirectory::new();
            fd.id = id;
            fd.path = path_clone.clone();
            fd.entries = files.into();
            match fd.write_to_bytes() {
                Ok(bytes) => Ok(bytes),
                Err(e) => Err(format!("serialize failed: {}", e)),
            }
        }
        Ok(Err(e)) => Err(format!("{}", e)),
        Err(e) => Err(format!("task failed: {}", e)),
    };

    let _ = tx.send(Data::AllFilesResult {
        id,
        conn_id,
        path: path_clone,
        result,
    });
}

#[cfg(not(any(target_os = "ios")))]
async fn read_empty_dirs(dir: &str, include_hidden: bool, tx: &UnboundedSender<Data>) {
    let path = dir.to_owned();
    let path_clone = dir.to_owned();

    let result = spawn_blocking(move || {
        let path_obj = Path::new(&path);
        let canonical = validate_and_canonicalize(&path_obj)?;
        let canonical_str = canonical.to_string_lossy().to_string();
        fs::get_empty_dirs_recursive(&canonical_str, include_hidden)
    })
    .await;

    match result {
        Ok(Ok(fds)) => {
            let mut msg_out = Message::new();
            let mut file_response = FileResponse::new();
            file_response.set_empty_dirs(ReadEmptyDirsResponse {
                path: path_clone,
                empty_dirs: fds,
                ..Default::default()
            });
            msg_out.set_file_response(file_response);
            send_raw(msg_out, tx);
        }
        Ok(Err(e)) => {
            log::error!("read_empty_dirs failed for '{}': {}", path_clone, e);
        }
        Err(e) => {
            log::error!("read_empty_dirs task failed for '{}': {}", path_clone, e);
        }
    }
}

#[cfg(not(any(target_os = "ios")))]
async fn read_dir(dir: &str, include_hidden: bool, tx: &UnboundedSender<Data>) {
    let path = {
        if dir.is_empty() {
            Config::get_home()
        } else {
            fs::get_path(dir)
        }
    };
    let result = spawn_blocking(move || {
        let canonical = validate_and_canonicalize(&path)?;
        fs::read_dir(&canonical, include_hidden)
    })
    .await;

    match result {
        Ok(Ok(fd)) => {
            let mut msg_out = Message::new();
            let mut file_response = FileResponse::new();
            file_response.set_dir(fd);
            msg_out.set_file_response(file_response);
            send_raw(msg_out, tx);
        }
        Ok(Err(e)) => {
            log::error!("read_dir failed for '{}': {}", dir, e);
        }
        Err(e) => {
            log::error!("read_dir task failed for '{}': {}", dir, e);
        }
    }
}

#[cfg(not(any(target_os = "ios")))]
async fn handle_result<F: std::fmt::Display, S: std::fmt::Display>(
    res: std::result::Result<std::result::Result<(), F>, S>,
    id: i32,
    file_num: i32,
    tx: &UnboundedSender<Data>,
) {
    match res {
        Err(err) => {
            send_raw(fs::new_error(id, err, file_num), tx);
        }
        Ok(Err(err)) => {
            send_raw(fs::new_error(id, err, file_num), tx);
        }
        Ok(Ok(())) => {
            send_raw(fs::new_done(id, file_num), tx);
        }
    }
}

#[cfg(not(any(target_os = "ios")))]
async fn remove_file(path: String, id: i32, file_num: i32, tx: &UnboundedSender<Data>) {
    handle_result(
        spawn_blocking(move || {
            let path_obj = Path::new(&path);
            let canonical = validate_and_canonicalize(&path_obj)?;
            let canonical_str = canonical.to_string_lossy().to_string();
            fs::remove_file(&canonical_str)
        })
        .await,
        id,
        file_num,
        tx,
    )
    .await;
}

#[cfg(not(any(target_os = "ios")))]
async fn create_dir(path: String, id: i32, tx: &UnboundedSender<Data>) {
    handle_result(
        spawn_blocking(move || {
            let path_obj = Path::new(&path);
            // For create_dir, check parent of the new directory
            let _canonical_parent = validate_parent_and_canonicalize(&path_obj)?;
            fs::create_dir(&path)
        })
        .await,
        id,
        0,
        tx,
    )
    .await;
}

#[cfg(not(any(target_os = "ios")))]
async fn rename_file(path: String, new_name: String, id: i32, tx: &UnboundedSender<Data>) {
    handle_result(
        spawn_blocking(move || {
            // Validate that new_name doesn't contain path traversal
            validate_file_name_no_traversal(&new_name)?;

            let path_obj = Path::new(&path);
            let canonical = validate_and_canonicalize(&path_obj)?;

            // Also validate that the new path would be in the same directory
            if let Some(parent) = canonical.parent() {
                let new_path = parent.join(&new_name);
                // Ensure new path is still under the same parent directory
                if let Ok(new_canonical) = new_path.canonicalize() {
                    if new_canonical.parent() != Some(parent) {
                        bail!("rename target is not in the same directory");
                    }
                } else {
                    // New path doesn't exist yet, just check the parent remains the same
                    if let Some(new_parent) = new_path.parent() {
                        if new_parent != parent {
                            bail!("rename target is not in the same directory");
                        }
                    }
                }
            }

            fs::rename_file(&path, &new_name)
        })
        .await,
        id,
        0,
        tx,
    )
    .await;
}

#[cfg(not(any(target_os = "ios")))]
async fn remove_dir(path: String, id: i32, recursive: bool, tx: &UnboundedSender<Data>) {
    let path = fs::get_path(&path);
    handle_result(
        spawn_blocking(move || {
            let canonical = validate_and_canonicalize(&path)?;
            if recursive {
                fs::remove_all_empty_dir(&canonical)
            } else {
                std::fs::remove_dir(&canonical).map_err(|err| err.into())
            }
        })
        .await,
        id,
        0,
        tx,
    )
    .await;
}

#[cfg(not(any(target_os = "ios")))]
fn send_raw(msg: Message, tx: &UnboundedSender<Data>) {
    match msg.write_to_bytes() {
        Ok(bytes) => {
            allow_err!(tx.send(Data::RawMessage(bytes)));
        }
        err => allow_err!(err),
    }
}

#[cfg(windows)]
fn cm_inner_send(id: i32, data: Data) {
    let lock = CLIENTS.read().unwrap();
    if id != 0 {
        if let Some(s) = lock.get(&id) {
            allow_err!(s.tx.send(data));
        }
    } else {
        for s in lock.values() {
            allow_err!(s.tx.send(data.clone()));
        }
    }
}

pub fn can_elevate() -> bool {
    #[cfg(windows)]
    return !crate::platform::is_installed();
    #[cfg(not(windows))]
    return false;
}

pub fn elevate_portable(_id: i32) {
    #[cfg(windows)]
    {
        let lock = CLIENTS.read().unwrap();
        if let Some(s) = lock.get(&_id) {
            allow_err!(s.tx.send(ipc::Data::DataPortableService(
                ipc::DataPortableService::RequestStart
            )));
        }
    }
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn handle_incoming_voice_call(id: i32, accept: bool) {
    if let Some(client) = CLIENTS.read().unwrap().get(&id) {
        // Not handled in iOS yet.
        #[cfg(not(any(target_os = "ios")))]
        allow_err!(client.tx.send(Data::VoiceCallResponse(accept)));
    };
}

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
#[inline]
pub fn close_voice_call(id: i32) {
    if let Some(client) = CLIENTS.read().unwrap().get(&id) {
        // Not handled in iOS yet.
        #[cfg(not(any(target_os = "ios")))]
        allow_err!(client.tx.send(Data::CloseVoiceCall("".to_owned())));
    };
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn quit_cm() {
    // in case of std::process::exit not work
    log::info!("quit cm");
    CLIENTS.write().unwrap().clear();
    crate::platform::quit_gui();
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ipc::Data;
    use hbb_common::{
        message_proto::{FileDirectory, FileResponse, Message},
        tokio::{runtime::Runtime, sync::mpsc::unbounded_channel},
    };
    use std::fs;

    /// On Windows, validate_and_canonicalize (and underlying ACL checks) should
    /// fail when the parent directory denies list/read access for the current
    /// user, even if the file itself is otherwise readable.
    ///
    /// This test manipulates real ACLs via icacls and relies on USERNAME availability.
    /// It is skipped if icacls is not available.
    #[test]
    #[cfg(windows)]
    fn validate_and_canonicalize_fails_when_parent_acl_denies_access() {
        use std::path::PathBuf;
        use std::process::Command;

        // Check if icacls is available, skip test if not
        let icacls_check = Command::new("icacls").arg("/?").output();
        if icacls_check.is_err() {
            eprintln!("Skipping ACL test: icacls binary not found");
            return;
        }

        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            // Prepare temporary directory layout:
            // base_dir/denied_dir/allowed_file.txt
            let mut base_dir = std::env::temp_dir();
            base_dir.push("rustdesk_ui_cm_interface_acl_test");
            let denied_dir: PathBuf = base_dir.join("denied_dir");
            let file_path: PathBuf = denied_dir.join("allowed_file.txt");

            let _ = fs::remove_dir_all(&base_dir); // best-effort cleanup
            fs::create_dir_all(&denied_dir).unwrap();
            fs::write(&file_path, b"hello").unwrap();

            // Deny read/list access on the parent directory for the current user.
            // This simulates the ACL-bypass scenario we want to guard against.
            let username = match std::env::var("USERNAME") {
                Ok(u) if !u.is_empty() => u,
                _ => {
                    // If we can't determine the username, skip the test.
                    eprintln!("USERNAME env var is not set, skipping ACL test");
                    return;
                }
            };

            let status = Command::new("icacls")
                .arg(&denied_dir)
                .arg("/deny")
                .arg(format!("{}:(RX)", username))
                .status()
                .expect("failed to run icacls /deny");

            if !status.success() {
                eprintln!(
                    "icacls /deny failed with status {:?}, skipping ACL test",
                    status
                );
                let _ = fs::remove_dir_all(&base_dir);
                return;
            }

            // Check opening the file directly still works (it should).
            let direct_open = fs::File::open(&file_path);
            assert!(
                direct_open.is_ok(),
                "expected direct file open to succeed despite parent ACL"
            );

            // Now any attempt to access a child under `denied_dir` via directory
            // listing should fail, and validate_and_canonicalize should surface
            // an error for the file path.
            let res = super::validate_and_canonicalize(&file_path);
            assert!(
                res.is_err(),
                "expected ACL validation failure for {:?}",
                file_path
            );

            // Best-effort cleanup: remove the deny ACE and test directory.
            let _ = Command::new("icacls")
                .arg(&denied_dir)
                .arg("/remove:d")
                .arg(&username)
                .status();
            let _ = fs::remove_dir_all(&base_dir);
        });
    }

    /// read_all_files should return an error result when the target path does not exist.
    #[test]
    #[cfg(not(any(target_os = "ios")))]
    fn read_all_files_nonexistent_path_returns_error_result() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, mut rx) = unbounded_channel();
            let path = "Z:/this_path_should_not_exist_for_rustdesk_tests";
            super::read_all_files(path.to_string(), false, 42, 7, &tx).await;

            let data = rx.recv().await.expect("expected AllFilesResult message");
            match data {
                Data::AllFilesResult {
                    id,
                    conn_id,
                    result,
                    ..
                } => {
                    assert_eq!(id, 42);
                    assert_eq!(conn_id, 7);
                    assert!(result.is_err());
                    let msg = result.err().unwrap();
                    // On failure we expect a canonicalize/IO related error message.
                    assert!(msg.contains("canonicalize failed") || msg.contains("task failed"));
                }
                other => panic!("unexpected data: {:?}", other),
            }
        });
    }

    /// read_all_files should succeed for a real, temporary directory and return
    /// a FileDirectory protobuf with matching id and path.
    #[test]
    #[cfg(not(any(target_os = "ios")))]
    fn read_all_files_success_for_temp_dir() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, mut rx) = unbounded_channel();

            // Create a temporary directory with a single file
            let mut dir = std::env::temp_dir();
            dir.push("rustdesk_ui_cm_interface_read_all_files_test");
            let _ = fs::remove_dir_all(&dir); // best-effort cleanup
            fs::create_dir_all(&dir).unwrap();
            let mut file_path = dir.clone();
            file_path.push("test.txt");
            fs::write(&file_path, b"hello").unwrap();

            let path_str = dir.to_string_lossy().to_string();
            super::read_all_files(path_str.clone(), false, 100, 9, &tx).await;

            let data = rx.recv().await.expect("expected AllFilesResult message");
            match data {
                Data::AllFilesResult {
                    id,
                    conn_id,
                    path,
                    result,
                } => {
                    assert_eq!(id, 100);
                    assert_eq!(conn_id, 9);
                    assert_eq!(path, path_str);
                    let bytes = result.expect("expected Ok bytes for temp dir");
                    let fd = FileDirectory::parse_from_bytes(&bytes)
                        .expect("failed to parse FileDirectory from bytes");
                    assert_eq!(fd.id, 100);
                    assert_eq!(fd.path, path_str);
                    // At least one entry (our test file) should be present.
                    assert!(!fd.entries.is_empty());
                }
                other => panic!("unexpected data: {:?}", other),
            }

            // best-effort cleanup
            let _ = fs::remove_dir_all(&dir);
        });
    }

    /// read_dir should send a FileResponse::dir for a real directory.
    #[test]
    #[cfg(not(any(target_os = "ios")))]
    fn read_dir_success_for_temp_dir() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, mut rx) = unbounded_channel();

            let mut dir = std::env::temp_dir();
            dir.push("rustdesk_ui_cm_interface_read_dir_test");
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();

            let path_str = dir.to_string_lossy().to_string();
            super::read_dir(&path_str, false, &tx).await;

            let data = rx.recv().await.expect("expected RawMessage from read_dir");
            let bytes = match data {
                Data::RawMessage(b) => b,
                other => panic!("unexpected data: {:?}", other),
            };

            let mut msg = Message::new();
            msg.merge_from_bytes(&bytes)
                .expect("failed to parse Message from RawMessage bytes");
            let file_resp: &FileResponse = msg.file_response();
            let dir_resp: &FileDirectory = file_resp.dir();

            // Canonicalization might change the path format (e.g., UNC paths on Windows)
            // so we just check that the path ends with the expected directory name
            assert!(dir_resp
                .path
                .ends_with("rustdesk_ui_cm_interface_read_dir_test"));

            let _ = fs::remove_dir_all(&dir);
        });
    }

    /// Test path traversal and security validation
    #[test]
    #[cfg(not(any(target_os = "ios")))]
    fn validate_file_name_security() {
        // Path traversal should be rejected
        assert!(super::validate_file_name_no_traversal("../etc/passwd").is_err());
        assert!(super::validate_file_name_no_traversal("foo/../bar").is_err());
        assert!(super::validate_file_name_no_traversal("..\\Windows\\System32").is_err());
        assert!(super::validate_file_name_no_traversal("..").is_err());

        // Absolute paths should be rejected
        assert!(super::validate_file_name_no_traversal("/etc/passwd").is_err());
        assert!(super::validate_file_name_no_traversal("\\Windows\\System32").is_err());
        #[cfg(windows)]
        assert!(super::validate_file_name_no_traversal("C:\\Windows").is_err());

        // Valid relative paths should be accepted
        assert!(super::validate_file_name_no_traversal("file.txt").is_ok());
        assert!(super::validate_file_name_no_traversal("subdir/file.txt").is_ok());
        assert!(super::validate_file_name_no_traversal("").is_ok());
    }

    /// Test transfer file validation
    #[test]
    #[cfg(not(any(target_os = "ios")))]
    fn validate_transfer_files_security() {
        // Valid files should pass
        let valid = vec![("file.txt".to_string(), 100u64)];
        assert!(super::validate_transfer_file_names(&valid).is_ok());

        // Path traversal should fail
        let invalid = vec![("../../../etc/passwd".to_string(), 100u64)];
        assert!(super::validate_transfer_file_names(&invalid).is_err());
    }
}
