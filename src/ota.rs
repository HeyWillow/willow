//! HTTP-to-OTA transfer and upgrade coordination.

use core::ffi::{c_char, c_void};
use core::fmt;
use core::slice;

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection, Method};
use esp_idf_svc::ota::{EspFirmwareInfoLoad, EspOta};
use esp_idf_sys::{
    CONFIG_FREERTOS_NO_AFFINITY, ESP_FAIL, EspError, esp_app_desc_t, esp_app_get_description,
    esp_http_client_is_complete_data_received, esp_image_header_t, esp_image_segment_header_t,
    esp_task_wdt_config_t, esp_task_wdt_reconfigure, xTaskCreatePinnedToCore,
};
use log::{debug, error, info, warn};

const BUFFER_SIZE: usize = 4096;
const HTTP_OK: u16 = 200;
const INSTALL_TASK_PRIORITY: u32 = 5;
const INSTALL_TASK_STACK_SIZE: u32 = 8_192;
const LOG_TARGET: &str = "WILLOW/OTA";
const SERVICE_SHUTDOWN_DELAY_MS: u32 = 1_000;
const TASK_CREATED: i32 = 1;
const USER_AGENT: &str = concat!("Willow/", env!("WILLOW_VERSION"));
const WATCHDOG_TIMEOUT_MS: u32 = 30_000;
const WRITE_DELAY_MS: u32 = 10;

#[derive(Debug)]
pub(crate) enum InstallError {
    Esp {
        operation: &'static str,
        source: EspError,
    },
    HttpStatus(u16),
    IncompleteHeader,
    IncompleteDownload,
}

impl InstallError {
    fn esp(operation: &'static str) -> impl FnOnce(EspError) -> Self {
        move |source| Self::Esp { operation, source }
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Esp { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::HttpStatus(status) => write!(formatter, "HTTP error ({status})"),
            Self::IncompleteHeader => formatter.write_str("firmware image header is incomplete"),
            Self::IncompleteDownload => formatter.write_str("firmware download is incomplete"),
        }
    }
}

fn fixed_c_string(value: &[c_char]) -> String {
    let bytes = unsafe { slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len()) };
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());

    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn adjust_watchdog() {
    // Erasing the full OTA partition can exceed the normal task watchdog
    // timeout. The device always restarts after this synchronous operation.
    let configuration = esp_task_wdt_config_t {
        timeout_ms: WATCHDOG_TIMEOUT_MS,
        idle_core_mask: 0,
        trigger_panic: true,
    };

    if let Some(error) =
        EspError::from(unsafe { esp_task_wdt_reconfigure(&raw const configuration) })
    {
        warn!(target: LOG_TARGET, "failed to adjust task watchdog: {error}");
    }
}

fn log_running_firmware(ota: &EspOta) {
    let boot_slot = ota.get_boot_slot();
    let running_slot = ota.get_running_slot();

    if let (Ok(boot), Ok(running)) = (&boot_slot, &running_slot)
        && boot.label != running.label
    {
        warn!(
            target: LOG_TARGET,
            "boot partition ({}) does not match running partition ({})",
            boot.label,
            running.label
        );
    }

    match running_slot {
        Ok(slot) => match slot.firmware {
            Some(firmware) => {
                info!(target: LOG_TARGET, "current firmware version: {}", firmware.version);
            }
            None => warn!(target: LOG_TARGET, "current firmware version is unavailable"),
        },
        Err(error) => {
            warn!(target: LOG_TARGET, "failed to read current firmware version: {error}");
        }
    }
}

fn read_image_header(
    connection: &mut EspHttpConnection,
    buffer: &mut [u8],
) -> Result<usize, InstallError> {
    let header_size = size_of::<esp_image_header_t>()
        + size_of::<esp_image_segment_header_t>()
        + size_of::<esp_app_desc_t>();
    let mut buffered = 0;

    while buffered < header_size {
        let read = connection
            .read(&mut buffer[buffered..])
            .map_err(InstallError::esp("failed to read firmware image header"))?;
        if read == 0 {
            return Err(InstallError::IncompleteHeader);
        }
        buffered += read;
    }

    let native_info = EspFirmwareInfoLoad
        .fetch_native(&buffer[..buffered])
        .ok_or(InstallError::IncompleteHeader)?;
    info!(
        target: LOG_TARGET,
        "new firmware version: {}",
        fixed_c_string(&native_info.app_desc.version)
    );

    Ok(buffered)
}

/// Returns the version embedded in the running application image.
pub(crate) fn running_version() -> String {
    let description = unsafe { &*esp_app_get_description() };
    fixed_c_string(&description.version)
}

/// Confirms the running firmware and cancels a pending rollback.
pub(crate) fn mark_running_slot_valid() -> Result<(), EspError> {
    let mut ota = EspOta::new()?;
    ota.mark_running_slot_valid()
}

/// Downloads, validates, and installs an OTA image synchronously.
pub(crate) fn install(url: &str) -> Result<(), InstallError> {
    info!(target: LOG_TARGET, "downloading OTA from {url}");

    let mut connection = EspHttpConnection::new(&Configuration::default())
        .map_err(InstallError::esp("failed to initialize HTTP client"))?;
    connection
        .initiate_request(Method::Get, url, &[("User-Agent", USER_AGENT)])
        .map_err(InstallError::esp("failed to open HTTP connection"))?;
    connection
        .initiate_response()
        .map_err(InstallError::esp("failed to fetch HTTP response headers"))?;

    let update_size = connection.header("Content-Length").unwrap_or("unknown");
    info!(target: LOG_TARGET, "update size: {update_size} byte");

    let status = connection.status();
    if status != HTTP_OK {
        return Err(InstallError::HttpStatus(status));
    }

    let mut ota = EspOta::new().map_err(InstallError::esp("failed to obtain OTA service"))?;
    log_running_firmware(&ota);

    // Keep the transfer buffer off the 8 KiB FreeRTOS OTA task stack.
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    let buffered = read_image_header(&mut connection, &mut buffer)?;

    // `initiate_update` deliberately uses OTA_SIZE_UNKNOWN, preserving the
    // previous full-partition erase. If any later operation fails, dropping
    // `update` invokes esp_ota_abort before this function returns.
    adjust_watchdog();
    info!(target: LOG_TARGET, "starting OTA");
    let mut update = ota
        .initiate_update()
        .map_err(InstallError::esp("failed to start OTA"))?;
    update
        .write(&buffer[..buffered])
        .map_err(InstallError::esp("failed to write OTA data"))?;

    let mut written = buffered;
    debug!(target: LOG_TARGET, "OTA data written: {written} byte");
    FreeRtos::delay_ms(WRITE_DELAY_MS);

    loop {
        let read = connection
            .read(&mut buffer)
            .map_err(InstallError::esp("failed to read firmware image"))?;
        if read == 0 {
            break;
        }

        update
            .write(&buffer[..read])
            .map_err(InstallError::esp("failed to write OTA data"))?;
        written += read;
        debug!(target: LOG_TARGET, "OTA data written: {written} byte");
        FreeRtos::delay_ms(WRITE_DELAY_MS);
    }

    if !unsafe { esp_http_client_is_complete_data_received(connection.handle()) } {
        return Err(InstallError::IncompleteDownload);
    }

    info!(target: LOG_TARGET, "OTA download completed");
    info!(target: LOG_TARGET, "total OTA data written: {written} byte");
    update
        .complete()
        .map_err(InstallError::esp("failed to complete OTA"))?;

    Ok(())
}

unsafe extern "C" fn install_task(data: *mut c_void) {
    // SAFETY: `start` transfers exactly one boxed String to a successfully
    // created task, and no other path reconstructs it after that success.
    let url = unsafe { Box::from_raw(data.cast::<String>()) };
    let installed = match install(&url) {
        Ok(()) => true,
        Err(error) => {
            error!(target: LOG_TARGET, "OTA failed: {error:?}");
            false
        }
    };
    drop(url);

    info!(
        target: LOG_TARGET,
        "OTA {}, restarting",
        if installed { "completed" } else { "failed" }
    );
    crate::ui::show_center_message(if installed {
        "Upgrade Done"
    } else {
        "Upgrade Failed"
    });
    crate::system::restart_delayed()
}

/// Stops active services and starts an owned OTA installation task.
pub(crate) fn start(url: &str) -> Result<(), EspError> {
    let url = Box::new(url.to_owned());
    let no_affinity = i32::try_from(CONFIG_FREERTOS_NO_AFFINITY)
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())?;

    if let Err(error) = crate::backlight::reset_display_timer(true) {
        error!(target: LOG_TARGET, "failed to pause display timer: {error:?}");
    }
    crate::ui::show_center_message("Starting Upgrade");
    crate::backlight::set(true, false);

    crate::audio::deinitialize();
    crate::was::deinitialize();

    FreeRtos::delay_ms(SERVICE_SHUTDOWN_DELAY_MS);

    let url = Box::into_raw(url);
    let status = unsafe {
        xTaskCreatePinnedToCore(
            Some(install_task),
            c"ota_task".as_ptr(),
            INSTALL_TASK_STACK_SIZE,
            url.cast(),
            INSTALL_TASK_PRIORITY,
            core::ptr::null_mut(),
            no_affinity,
        )
    };
    if status == TASK_CREATED {
        Ok(())
    } else {
        unsafe { drop(Box::from_raw(url)) };
        let error = EspError::from_infallible::<ESP_FAIL>();
        error!(target: LOG_TARGET, "failed to start OTA task: {error:?}; restarting");
        crate::ui::show_center_message("Upgrade Failed");
        // Task creation just failed, so recover on this surviving caller
        // instead of relying on another task to perform the restart.
        crate::system::restart_delayed()
    }
}
