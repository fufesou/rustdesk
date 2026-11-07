use super::{
    display_name, handles::Printer, owner_marker, port_name, ports, queues, Queue, DRIVER_NAME,
};
use hbb_common::{anyhow::Context, bail, ResultType};
use windows::{
    core::{w, HSTRING, PCWSTR, PWSTR},
    Win32::Graphics::Printing::{
        AddPrinterW, DeletePrinter, SetPrinterW, PRINTER_ALL_ACCESS,
        PRINTER_ATTRIBUTE_KEEPPRINTEDJOBS, PRINTER_ATTRIBUTE_LOCAL, PRINTER_HANDLE,
        PRINTER_INFO_2W,
    },
};

const QUEUE_INFO_LEVEL: u32 = 2;
const DEFAULT_PRIORITY: u32 = 1;
const UPDATE_PRINTER: u32 = 0;

pub fn install(app_name: &str, instance_id: &str) -> ResultType<Queue> {
    if let Some(queue) = queues::installed(app_name)? {
        return Ok(queue);
    }
    let queue = Queue {
        name: display_name(app_name)?,
        port: port_name(instance_id)?,
    };
    let existing = queues::get(&queue.name)?;
    if let Some(info) = &existing {
        info.check_migration(app_name)?;
    }
    ports::add(&queue.port).context("Failed to add the PDF Local Port")?;
    let result = if existing.is_some() {
        replace_legacy(&queue, app_name)
    } else {
        add_queue(&queue, &owner_marker(app_name)?)
    };
    if let Err(error) = result {
        if let Err(cleanup) = ports::remove(&queue.port) {
            bail!("PDF queue installation failed: {error:#}; port cleanup failed: {cleanup:#}");
        }
        return Err(
            error.context("Microsoft Print To PDF must be installed and allowed by system policy")
        );
    }
    Ok(queue)
}

pub fn uninstall(app_name: &str) -> ResultType<()> {
    let queue = if let Some(queue) = queues::installed(app_name)? {
        queue
    } else {
        let Some(info) =
            queues::get(&display_name(app_name)?)?.filter(|info| info.is_legacy(app_name))
        else {
            return Ok(());
        };
        Queue {
            name: info.name,
            port: info.port,
        }
    };
    let printer = removable_printer(&queue, app_name)?;
    unsafe { DeletePrinter(printer.0)? };
    drop(printer);
    ports::remove(&queue.port)
        .context("The PDF queue was removed, but its port could not be removed")
}

fn removable_printer(queue: &Queue, app_name: &str) -> ResultType<Printer> {
    let printer = Printer::open(&queue.name, PRINTER_ALL_ACCESS)?;
    let info = queues::get(&queue.name)?.context("The PDF queue was removed during uninstall")?;
    if !info.is_legacy(app_name) {
        queues::validate(&info)?;
        if info.comment != owner_marker(app_name)? {
            bail!("The PDF queue changed ownership during uninstall");
        }
    }
    if info.port != queue.port {
        bail!("The PDF queue changed ownership during uninstall");
    }
    if info.jobs != 0 {
        bail!("The PDF queue still contains jobs; finish or cancel them before uninstalling");
    }
    Ok(printer)
}

fn replace_legacy(queue: &Queue, app_name: &str) -> ResultType<()> {
    let printer = Printer::open(&queue.name, PRINTER_ALL_ACCESS)?;
    let buffer = queues::read(&printer)?;
    let mut info = unsafe { buffer.item::<PRINTER_INFO_2W>(0)? };
    queues::info_from(info)?.check_migration(app_name)?;
    info.pSecurityDescriptor = Default::default();
    let port = HSTRING::from(&queue.port);
    let driver = HSTRING::from(DRIVER_NAME);
    let comment = HSTRING::from(owner_marker(app_name)?);
    info.pPortName = PWSTR(port.as_ptr().cast_mut());
    info.pDriverName = PWSTR(driver.as_ptr().cast_mut());
    info.pComment = PWSTR(comment.as_ptr().cast_mut());
    // Driver-specific settings cannot be reused with the inbox PDF driver.
    info.pDevMode = std::ptr::null_mut();
    info.Attributes |= PRINTER_ATTRIBUTE_KEEPPRINTEDJOBS;
    unsafe {
        SetPrinterW(
            printer.0,
            QUEUE_INFO_LEVEL,
            Some((&info as *const PRINTER_INFO_2W).cast()),
            UPDATE_PRINTER,
        )?;
    }
    Ok(())
}

fn add_queue(queue: &Queue, comment: &str) -> ResultType<()> {
    let name = HSTRING::from(&queue.name);
    let port = HSTRING::from(&queue.port);
    let driver = HSTRING::from(DRIVER_NAME);
    let comment = HSTRING::from(comment);
    let info = PRINTER_INFO_2W {
        pPrinterName: PWSTR(name.as_ptr().cast_mut()),
        pPortName: PWSTR(port.as_ptr().cast_mut()),
        pDriverName: PWSTR(driver.as_ptr().cast_mut()),
        pComment: PWSTR(comment.as_ptr().cast_mut()),
        pPrintProcessor: PWSTR(w!("WinPrint").as_ptr().cast_mut()),
        pDatatype: PWSTR(w!("RAW").as_ptr().cast_mut()),
        Attributes: PRINTER_ATTRIBUTE_LOCAL | PRINTER_ATTRIBUTE_KEEPPRINTEDJOBS,
        Priority: DEFAULT_PRIORITY,
        DefaultPriority: DEFAULT_PRIORITY,
        ..Default::default()
    };
    let handle = unsafe {
        AddPrinterW(
            PCWSTR::null(),
            QUEUE_INFO_LEVEL,
            (&info as *const PRINTER_INFO_2W).cast(),
        )?
    };
    let _printer = Printer(PRINTER_HANDLE { Value: handle.0 });
    Ok(())
}
