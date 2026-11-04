use super::{AuthConnType, Connection, AUTHED_CONNS, CONTROL_PERMISSIONS_ARRAY};
use crate::{ipc::Data, remote_print::job};
use base::{
    config::keys,
    fs,
    message_proto::{file_transfer_send_request, Message, MessageBox},
};
use hbb_common::{
    anyhow::anyhow,
    bail,
    tokio::{sync::mpsc, time::Instant},
    ResultType,
};
use std::time::Duration;

const OFFER_LIFETIME: Duration = Duration::from_secs(60);

pub(crate) struct PdfTarget {
    id: i32,
    sender: mpsc::UnboundedSender<Data>,
}

pub(crate) fn pdf_printer_target() -> ResultType<PdfTarget> {
    let connections = AUTHED_CONNS.lock().unwrap();
    let connection = job::unique_target(connections.iter().filter(|connection| {
        connection.conn_type == AuthConnType::Remote
            && connection.pdf_printer
            && !connection.sender.is_closed()
            && permitted(connection.conn_id)
    }))?;
    Ok(PdfTarget {
        id: connection.conn_id,
        sender: connection.sender.clone(),
    })
}

fn permitted(id: i32) -> bool {
    let permission = CONTROL_PERMISSIONS_ARRAY
        .lock()
        .unwrap()
        .iter()
        .find(|(connection_id, _)| *connection_id == id)
        .map(|(_, permissions)| permissions.clone());
    Connection::permission(keys::OPTION_ENABLE_REMOTE_PRINTER, &permission)
}

impl PdfTarget {
    pub(crate) fn authorized(&self) -> bool {
        if self.sender.is_closed() || !permitted(self.id) {
            return false;
        }
        AUTHED_CONNS.lock().unwrap().iter().any(|connection| {
            connection.conn_id == self.id
                && connection.pdf_printer
                && connection.sender.same_channel(&self.sender)
        })
    }

    pub(crate) fn send(&self, data: Vec<u8>) -> ResultType<()> {
        if !self.authorized() {
            bail!("The bound PDF receiver disconnected or lost printing permission");
        }
        self.sender
            .send(Data::PdfPrinterData(data))
            .map_err(|_| anyhow!("The bound PDF receiver disconnected"))
    }

    pub(crate) fn report_error(&self, error: &str) {
        if let Err(send_error) = self.sender.send(Data::PdfPrinterError(error.to_owned())) {
            hbb_common::log::trace!(
                "PDF capture error could not reach the closed session: {send_error}"
            );
        }
    }
}

impl Connection {
    pub(super) async fn send_pdf_request(&mut self, data: Vec<u8>) {
        if !self.lr.pdf_printing_supported
            || !Self::permission(
                keys::OPTION_ENABLE_REMOTE_PRINTER,
                &self.control_permissions,
            )
        {
            self.send_remote_printing_disallowed().await;
            return;
        }
        let path = job::new_request_path();
        let mut message = fs::new_send(0, fs::JobType::Printer, path.clone(), 1, false);
        message.mut_file_action().mut_send().print_format =
            file_transfer_send_request::PrintFormat::Pdf.into();
        self.printer_data
            .retain(|(created, _, _)| created.elapsed() < OFFER_LIFETIME);
        self.printer_data.push((Instant::now(), path, data));
        self.send(message).await;
    }

    pub(super) async fn send_pdf_error(&mut self, error: String) {
        let mut message = Message::new();
        message.set_message_box(MessageBox {
            msgtype: "error".to_owned(),
            title: "Remote printing".to_owned(),
            text: error,
            ..Default::default()
        });
        self.send(message).await;
    }

    pub(super) fn check_pdf_download(&self, path: &str) -> ResultType<()> {
        if !job::is_pdf_request_path(path) {
            return Ok(());
        }
        if !self.lr.pdf_printing_supported
            || !Self::permission(
                keys::OPTION_ENABLE_REMOTE_PRINTER,
                &self.control_permissions,
            )
        {
            bail!("PDF printing is no longer permitted for this session");
        }
        if !self
            .printer_data
            .iter()
            .any(|(_, pending, _)| pending == path)
        {
            bail!("The PDF print request expired or was already consumed");
        }
        Ok(())
    }
}
