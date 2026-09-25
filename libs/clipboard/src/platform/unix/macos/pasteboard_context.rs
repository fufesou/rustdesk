use super::{
    item_data_provider::{create_pasteboard_file_url_provider, PasteboardFileUrlProvider},
    paste_observer::PasteObserver,
    paste_task::{FileContentsResponse, PasteTask},
};
use crate::{
    platform::unix::{
        filetype::FileDescription, FILECONTENTS_FORMAT_NAME, FILEDESCRIPTORW_FORMAT_NAME,
    },
    send_data, ClipboardFile, CliprdrError, CliprdrServiceContext, ProgressPercent,
};
use hbb_common::{bail, log, ResultType};
use objc2::{
    class, msg_send, msg_send_id,
    rc::{autoreleasepool, Id},
    runtime::{NSObject, ProtocolObject},
    sel, ClassType,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
use objc2_foundation::{NSArray, NSString};
use std::{
    io,
    path::Path,
    sync::{
        mpsc::{channel, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

lazy_static::lazy_static! {
    static ref PASTE_OBSERVER_INFO: Arc<Mutex<Option<PasteObserverInfo>>> = Default::default();
}

pub const TEMP_FILE_PREFIX: &str = ".rustdesk_";
const CLIPBOARD_CHECK_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Default, Debug, Clone, PartialEq)]
pub(super) struct PasteObserverInfo {
    pub file_descriptor_id: i32,
    pub conn_id: i32,
    pub source_path: String,
    pub target_path: String,
    pub pasteboard_change_count: isize,
}

enum ClipboardUpdate {
    Set(PasteObserverInfo),
    Clear(i32),
}

impl PasteObserverInfo {
    fn exit_msg() -> Self {
        Self::default()
    }
}

struct ContextInfo {
    tx: Sender<io::Result<PasteObserverInfo>>,
    handle: thread::JoinHandle<()>,
}

pub struct PasteboardContext {
    pasteboard: Id<NSPasteboard>,
    observer: Arc<Mutex<PasteObserver>>,
    tx_handle: Option<ContextInfo>,
    tx_remove_file: Option<Sender<ClipboardUpdate>>,
    remove_file_handle: Option<thread::JoinHandle<()>>,
    tx_paste_task: Sender<FileContentsResponse>,
    paste_task: Arc<Mutex<PasteTask>>,
}

unsafe impl Send for PasteboardContext {}
unsafe impl Sync for PasteboardContext {}

impl Drop for PasteboardContext {
    fn drop(&mut self) {
        self.observer.lock().unwrap().stop();
        if let Some(tx_handle) = self.tx_handle.take() {
            if tx_handle.tx.send(Ok(PasteObserverInfo::exit_msg())).is_ok() {
                tx_handle.handle.join().ok();
            }
        }
        self.tx_remove_file.take();
        if let Some(handle) = self.remove_file_handle.take() {
            handle.join().ok();
        }
        PASTE_OBSERVER_INFO.lock().unwrap().take();
    }
}

impl CliprdrServiceContext for PasteboardContext {
    fn set_is_stopped(&mut self) -> Result<(), CliprdrError> {
        Ok(())
    }

    fn empty_clipboard(&mut self, conn_id: i32) -> Result<bool, CliprdrError> {
        Ok(self.empty_clipboard_(conn_id))
    }

    fn server_clip_file(&mut self, conn_id: i32, msg: ClipboardFile) -> Result<(), CliprdrError> {
        self.server_clip_file_(conn_id, msg)
    }

    fn get_progress_percent(&self) -> Option<ProgressPercent> {
        self.paste_task.lock().unwrap().progress_percent()
    }

    fn cancel(&mut self) {
        self.paste_task.lock().unwrap().cancel();
    }
}

impl PasteboardContext {
    fn init(&mut self) {
        let (tx_remove_file, rx_remove_file) = channel();
        let name = autoreleasepool(|_| unsafe { self.pasteboard.name().to_string() });
        let handle_remove_file =
            Self::init_thread_remove_file(rx_remove_file, name, self.observer.clone());
        self.tx_remove_file = Some(tx_remove_file.clone());
        self.remove_file_handle = Some(handle_remove_file);

        let (tx, rx) = channel();
        let handle = Self::init_thread_observer(tx_remove_file, rx);
        self.tx_handle = Some(ContextInfo { tx, handle });
    }

    fn init_thread_observer(
        tx_remove_file: Sender<ClipboardUpdate>,
        rx: Receiver<io::Result<PasteObserverInfo>>,
    ) -> thread::JoinHandle<()> {
        let exit_msg = PasteObserverInfo::exit_msg();
        thread::spawn(move || loop {
            match rx.recv() {
                Ok(Ok(task_info)) => {
                    if task_info == exit_msg {
                        log::debug!("pasteboard item data provider: exit");
                        break;
                    }
                    tx_remove_file.send(ClipboardUpdate::Set(task_info)).ok();
                }
                Ok(Err(e)) => {
                    log::error!("pasteboard item data provider, inner error: {e}");
                }
                Err(e) => {
                    log::error!("pasteboard item data provider, error: {e}");
                    break;
                }
            }
        })
    }

    fn init_thread_remove_file(
        rx: Receiver<ClipboardUpdate>,
        name: String,
        observer: Arc<Mutex<PasteObserver>>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let pasteboard = autoreleasepool(|_| unsafe {
                NSPasteboard::pasteboardWithName(&NSString::from_str(&name))
            });
            let mut current: Option<PasteObserverInfo> = None;
            loop {
                let update = if current.is_some() {
                    rx.recv_timeout(CLIPBOARD_CHECK_INTERVAL)
                } else {
                    rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                };
                let disconnected = matches!(&update, Err(RecvTimeoutError::Disconnected));
                let change_count = autoreleasepool(|_| unsafe { pasteboard.changeCount() });
                let clear = match update {
                    Ok(ClipboardUpdate::Set(info)) => {
                        if info.pasteboard_change_count != change_count {
                            Self::remove_placeholder(&info.source_path);
                            continue;
                        }
                        current = Self::set_clipboard_file(current.take(), info, &observer);
                        false
                    }
                    Ok(ClipboardUpdate::Clear(conn_id)) => current
                        .as_ref()
                        .map(|info| conn_id == 0 || info.conn_id == conn_id)
                        .unwrap_or(false),
                    Err(RecvTimeoutError::Timeout) => current
                        .as_ref()
                        .map(|info| info.pasteboard_change_count != change_count)
                        .unwrap_or(false),
                    Err(RecvTimeoutError::Disconnected) => true,
                };
                if clear {
                    if let Some(info) = current.take() {
                        Self::release_clipboard_file(&pasteboard, &observer, info);
                    }
                }
                if disconnected {
                    break;
                }
            }
        })
    }

    fn set_clipboard_file(
        current: Option<PasteObserverInfo>,
        info: PasteObserverInfo,
        observer: &Mutex<PasteObserver>,
    ) -> Option<PasteObserverInfo> {
        if let Some(previous) = current.as_ref() {
            // The provider can supply the URL before writeObjects returns.
            if previous.pasteboard_change_count == info.pasteboard_change_count
                && info.source_path.is_empty()
            {
                return current;
            }
            if previous.source_path != info.source_path {
                Self::remove_placeholder(&previous.source_path);
            }
        }
        observer.lock().unwrap().start(info.clone());
        Some(info)
    }

    fn release_clipboard_file(
        pasteboard: &NSPasteboard,
        observer: &Mutex<PasteObserver>,
        info: PasteObserverInfo,
    ) {
        autoreleasepool(|_| unsafe {
            if pasteboard.changeCount() == info.pasteboard_change_count {
                pasteboard.clearContents();
            }
        });
        observer.lock().unwrap().stop();
        Self::remove_placeholder(&info.source_path);
    }

    fn remove_placeholder(path: &str) {
        if !path.is_empty() {
            if let Err(error) = std::fs::remove_file(path) {
                if error.kind() != io::ErrorKind::NotFound {
                    hbb_common::throttled_log!(
                        CLIPBOARD_CHECK_INTERVAL,
                        warn,
                        "Failed to remove clipboard placeholder {path}: {error}"
                    );
                }
            }
        }
    }

    fn empty_clipboard_(&mut self, conn_id: i32) -> bool {
        self.tx_remove_file
            .as_ref()
            .map(|tx| tx.send(ClipboardUpdate::Clear(conn_id)).ok());
        let mut pending = PASTE_OBSERVER_INFO.lock().unwrap();
        if pending
            .as_ref()
            .map(|info| conn_id == 0 || info.conn_id == conn_id)
            .unwrap_or(false)
        {
            pending.take();
        }
        true
    }

    fn temp_files_count() -> usize {
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir("/tmp") {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if path.is_file() {
                        if let Some(file_name) = path.file_name() {
                            if let Some(file_name_str) = file_name.to_str() {
                                if file_name_str.starts_with(TEMP_FILE_PREFIX) {
                                    count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        count
    }

    fn server_clip_file_(&mut self, conn_id: i32, msg: ClipboardFile) -> Result<(), CliprdrError> {
        match msg {
            ClipboardFile::FormatList { format_list } => {
                let temp_files = Self::temp_files_count();
                if temp_files >= 3 {
                    // The temp files should be 0 or 1 in normal case.
                    // We should not continue to paste files if there are more than 3 temp files.
                    return Err(CliprdrError::CommonError {
                        description: format!(
                            "too many temp files, current: {}, limit: {}",
                            temp_files, 3
                        ),
                    });
                }

                let task_lock = self.paste_task.lock().unwrap();
                if !task_lock.is_finished() {
                    return Err(CliprdrError::CommonError {
                        description: "previous file paste task is not finished".to_string(),
                    });
                }
                self.handle_format_list(conn_id, format_list)?;
            }
            ClipboardFile::FormatDataResponse {
                msg_flags,
                format_data,
            } => {
                self.handle_format_data_response(conn_id, msg_flags, format_data)?;
            }
            ClipboardFile::FileContentsResponse {
                msg_flags,
                stream_id,
                requested_data,
            } => {
                self.handle_file_contents_response(conn_id, msg_flags, stream_id, requested_data)?;
            }
            ClipboardFile::TryEmpty => self.handle_try_empty(conn_id),
            _ => {}
        }
        Ok(())
    }

    fn handle_format_list(
        &self,
        conn_id: i32,
        format_list: Vec<(i32, String)>,
    ) -> Result<(), CliprdrError> {
        if let Some(tx_handle) = self.tx_handle.as_ref() {
            if !format_list
                .iter()
                .find(|(_, name)| name == FILECONTENTS_FORMAT_NAME)
                .map(|(id, _)| *id)
                .is_some()
            {
                return Err(CliprdrError::CommonError {
                    description: "no file contents format found".to_string(),
                });
            };
            let Some(file_descriptor_id) = format_list
                .iter()
                .find(|(_, name)| name == FILEDESCRIPTORW_FORMAT_NAME)
                .map(|(id, _)| *id)
            else {
                return Err(CliprdrError::CommonError {
                    description: "no file descriptor format found".to_string(),
                });
            };

            autoreleasepool(|_| self.set_clipboard_item(tx_handle, conn_id, file_descriptor_id))?;
        } else {
            return Err(CliprdrError::CommonError {
                description: "pasteboard context is not inited".to_string(),
            });
        }
        Ok(())
    }

    fn set_clipboard_item(
        &self,
        tx_handle: &ContextInfo,
        conn_id: i32,
        file_descriptor_id: i32,
    ) -> Result<(), CliprdrError> {
        let tx = tx_handle.tx.clone();
        let task_info = PasteObserverInfo {
            file_descriptor_id,
            conn_id,
            source_path: "".to_string(),
            target_path: "".to_string(),
            pasteboard_change_count: unsafe { self.pasteboard.clearContents() },
        };
        let provider = create_pasteboard_file_url_provider(task_info.clone(), tx);
        unsafe {
            let types = NSArray::from_vec(vec![NSString::from_str(
                &NSPasteboardTypeFileURL.to_string(),
            )]);
            let item = objc2_app_kit::NSPasteboardItem::new();
            item.setDataProvider_forTypes(&ProtocolObject::from_id(provider), &types);
            if !self
                .pasteboard
                .writeObjects(&Id::cast(NSArray::from_vec(vec![item])))
            {
                return Err(CliprdrError::CommonError {
                    description: "failed to write objects".to_string(),
                });
            }
        }
        if let Some(tx) = self.tx_remove_file.as_ref() {
            tx.send(ClipboardUpdate::Set(task_info)).map_err(|error| {
                CliprdrError::CommonError {
                    description: error.to_string(),
                }
            })?;
        }
        Ok(())
    }

    fn handle_format_data_response(
        &self,
        conn_id: i32,
        msg_flags: i32,
        format_data: Vec<u8>,
    ) -> Result<(), CliprdrError> {
        log::debug!("handle format data response, msg_flags: {msg_flags}");
        if msg_flags != 0x1 {
            // return failure message?
        }

        let mut task_lock = self.paste_task.lock().unwrap();
        let target_dir = {
            let mut pending = PASTE_OBSERVER_INFO.lock().unwrap();
            if pending.as_ref().is_some_and(|task| task.conn_id == conn_id) {
                pending.take().map(|task| task.target_path)
            } else {
                None
            }
        };
        // unreachable in normal case
        let Some(target_dir) = target_dir.as_ref().map(|d| Path::new(d).parent()).flatten() else {
            return Err(CliprdrError::CommonError {
                description: "failed to get parent path".to_string(),
            });
        };
        // unreachable in normal case
        if !target_dir.exists() {
            return Err(CliprdrError::CommonError {
                description: "target path does not exist".to_string(),
            });
        }
        let target_dir = target_dir.to_owned();
        match FileDescription::parse_file_descriptors(format_data, conn_id) {
            Ok(files) => {
                task_lock.start(target_dir, files);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn handle_file_contents_response(
        &self,
        conn_id: i32,
        msg_flags: i32,
        stream_id: i32,
        requested_data: Vec<u8>,
    ) -> Result<(), CliprdrError> {
        log::debug!("handle file contents response");
        self.tx_paste_task
            .send(FileContentsResponse {
                conn_id,
                msg_flags,
                stream_id,
                requested_data,
            })
            .ok();
        Ok(())
    }

    fn handle_try_empty(&mut self, conn_id: i32) {
        log::debug!("empty_clipboard called");
        let ret = self.empty_clipboard_(conn_id);
        log::debug!(
            "empty_clipboard called, conn_id {}, return {}",
            conn_id,
            ret
        );
    }
}

fn handle_paste_result(task_info: &PasteObserverInfo) {
    log::info!(
        "file {} is pasted to {}",
        &task_info.source_path,
        &task_info.target_path
    );
    if Path::new(&task_info.target_path).parent().is_none() {
        log::error!(
            "failed to get parent path of {}, no need to perform pasting",
            &task_info.target_path
        );
        return;
    }

    std::fs::remove_file(&task_info.target_path).ok();
    let mut pending = PASTE_OBSERVER_INFO.lock().unwrap();
    if pending.is_some() {
        hbb_common::throttled_log!(
            CLIPBOARD_CHECK_INTERVAL,
            warn,
            "Previous paste request is not finished, ignore new request."
        );
        return;
    }
    pending.replace(task_info.clone());
    let data = ClipboardFile::FormatDataRequest {
        requested_format_id: task_info.file_descriptor_id,
    };
    if let Err(error) = send_data(task_info.conn_id as _, data) {
        pending.take();
        hbb_common::throttled_log!(
            CLIPBOARD_CHECK_INTERVAL,
            warn,
            "Failed to request clipboard file descriptors: {error}"
        );
    }
}

#[inline]
pub fn create_pasteboard_context() -> ResultType<Box<PasteboardContext>> {
    static EXIT_OBSERVER: OnceLock<bool> = OnceLock::new();
    if !*EXIT_OBSERVER.get_or_init(|| {
        autoreleasepool(|_| unsafe {
            let center: Option<Id<NSObject>> =
                msg_send_id![class!(NSNotificationCenter), defaultCenter];
            let Some(center) = center else {
                return false;
            };
            // The class remains alive even after the pasteboard releases its provider.
            let observer = PasteboardFileUrlProvider::class() as *const _ as *const NSObject;
            let _: () = msg_send![&*center,
                addObserver: observer,
                selector: sel!(applicationWillTerminate:),
                name: &*NSString::from_str("NSApplicationWillTerminateNotification"),
                object: std::ptr::null::<NSObject>()
            ];
            true
        })
    }) {
        bail!("failed to register clipboard exit cleanup");
    }
    let pasteboard: Option<Id<NSPasteboard>> =
        unsafe { msg_send_id![NSPasteboard::class(), generalPasteboard] };
    let Some(pasteboard) = pasteboard else {
        bail!("failed to get general pasteboard");
    };
    let mut observer = PasteObserver::new();
    observer.init(handle_paste_result)?;
    let (tx, rx) = channel();
    let mut context = Box::new(PasteboardContext {
        pasteboard,
        observer: Arc::new(Mutex::new(observer)),
        tx_handle: None,
        tx_remove_file: None,
        remove_file_handle: None,
        tx_paste_task: tx,
        paste_task: Arc::new(Mutex::new(PasteTask::new(rx))),
    });
    context.init();
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::Instant};

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    const TEST_CONN_ID: i32 = -11;
    const CONTENTS_FORMAT: i32 = 1;
    const DESCRIPTOR_FORMAT: i32 = 2;
    const ORIGINAL_TIMEOUT_ELAPSED: Duration = Duration::from_secs(31);
    const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
    const POLL_INTERVAL: Duration = Duration::from_millis(25);

    fn test_context() -> PasteboardContext {
        let (tx, rx) = channel();
        let mut observer = PasteObserver::new();
        observer.init(handle_paste_result).unwrap();
        let mut context = PasteboardContext {
            pasteboard: unsafe { NSPasteboard::pasteboardWithUniqueName() },
            observer: Arc::new(Mutex::new(observer)),
            tx_handle: None,
            tx_remove_file: None,
            remove_file_handle: None,
            tx_paste_task: tx,
            paste_task: Arc::new(Mutex::new(PasteTask::new(rx))),
        };
        context.init();
        context
    }

    fn publish_files(context: &PasteboardContext) {
        context
            .handle_format_list(
                TEST_CONN_ID,
                vec![
                    (CONTENTS_FORMAT, FILECONTENTS_FORMAT_NAME.to_owned()),
                    (DESCRIPTOR_FORMAT, FILEDESCRIPTORW_FORMAT_NAME.to_owned()),
                ],
            )
            .unwrap();
    }

    fn placeholder(context: &PasteboardContext) -> PathBuf {
        let url = unsafe {
            context
                .pasteboard
                .stringForType(NSPasteboardTypeFileURL)
                .unwrap()
                .to_string()
        };
        PathBuf::from(url.strip_prefix("file://").unwrap())
    }

    fn wait_until(done: impl Fn() -> bool) {
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        while !done() {
            assert!(
                Instant::now() < deadline,
                "clipboard cleanup did not finish"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn placeholder_survives_delay_until_clipboard_replacement() {
        use objc2_app_kit::NSPasteboardTypeString;

        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        autoreleasepool(|_| unsafe {
            let context = test_context();
            publish_files(&context);
            let source = placeholder(&context);
            thread::sleep(ORIGINAL_TIMEOUT_ELAPSED);
            assert!(source.is_file(), "the clipboard URL must not expire");
            let pasteboard = context.pasteboard.clone();
            pasteboard.clearContents();
            assert!(pasteboard.setString_forType(
                &NSString::from_str("replacement clipboard"),
                NSPasteboardTypeString,
            ));
            wait_until(|| !source.exists());
            drop(context);
            assert_eq!(
                pasteboard
                    .stringForType(NSPasteboardTypeString)
                    .unwrap()
                    .to_string(),
                "replacement clipboard"
            );
            let _: () = objc2::msg_send![&*pasteboard, releaseGlobally];
        });
    }

    #[test]
    fn placeholder_survives_paste_and_is_removed_on_invalidation() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        autoreleasepool(|_| unsafe {
            let mut context = test_context();
            publish_files(&context);
            let source = placeholder(&context);
            for _ in 0..2 {
                let target = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
                std::fs::copy(&source, &target).unwrap();
                handle_paste_result(&PasteObserverInfo {
                    conn_id: TEST_CONN_ID,
                    file_descriptor_id: DESCRIPTOR_FORMAT,
                    source_path: source.to_string_lossy().into_owned(),
                    target_path: target.to_string_lossy().into_owned(),
                    ..Default::default()
                });
                assert!(source.is_file(), "pasting must retain the clipboard source");
                assert!(!target.exists());
                assert_eq!(placeholder(&context), source);
            }
            context.empty_clipboard_(TEST_CONN_ID);
            wait_until(|| !source.exists());
            assert!(context
                .pasteboard
                .stringForType(NSPasteboardTypeFileURL)
                .is_none());
            publish_files(&context);
            context.empty_clipboard_(TEST_CONN_ID);
            wait_until(|| {
                context
                    .pasteboard
                    .stringForType(NSPasteboardTypeFileURL)
                    .is_none()
            });
            publish_files(&context);
            let next_source = placeholder(&context);
            let pasteboard = context.pasteboard.clone();
            drop(context);
            wait_until(|| !next_source.exists());
            assert!(pasteboard.stringForType(NSPasteboardTypeFileURL).is_none());
            let _: () = objc2::msg_send![&*pasteboard, releaseGlobally];
        });
    }

    #[test]
    fn test_temp_files_count() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut c = super::PasteboardContext::temp_files_count();

        let mut created_files = vec![];
        for _ in 0..10 {
            let path = format!(
                "/tmp/{}{}",
                super::TEMP_FILE_PREFIX,
                uuid::Uuid::new_v4().to_string()
            );
            if std::fs::File::create(&path).is_ok() {
                created_files.push(path);
                c += 1;
            }
        }

        assert_eq!(c, super::PasteboardContext::temp_files_count());

        // Clean up the created files.
        for file in created_files {
            std::fs::remove_file(&file).ok();
        }
    }
}
