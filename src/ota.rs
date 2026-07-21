//! Synchronous HTTP-to-OTA transfer engine.

use core::ffi::{CStr, c_char};
use core::fmt;
use core::slice;

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection, Method};
use esp_idf_svc::ota::{EspFirmwareInfoLoad, EspOta};
use esp_idf_sys::{
    EspError, esp_app_desc_t, esp_http_client_is_complete_data_received, esp_image_header_t,
    esp_image_segment_header_t, esp_task_wdt_config_t, esp_task_wdt_reconfigure,
};
use log::{debug, error, info, warn};

const BUFFER_SIZE: usize = 4096;
const HTTP_OK: u16 = 200;
const LOG_TARGET: &str = "WILLOW/OTA";
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

    if let Some(error) = EspError::from(unsafe { esp_task_wdt_reconfigure(&configuration) }) {
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
                info!(target: LOG_TARGET, "current firmware version: {}", firmware.version)
            }
            None => warn!(target: LOG_TARGET, "current firmware version is unavailable"),
        },
        Err(error) => {
            warn!(target: LOG_TARGET, "failed to read current firmware version: {error}")
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
    // `update` invokes esp_ota_abort before this function returns to C.
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

/// Converts the borrowed C URL for the Rust OTA installer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ota_install(url: *const c_char) -> bool {
    if url.is_null() {
        error!(target: LOG_TARGET, "OTA URL is null");
        return false;
    }

    let Ok(url) = unsafe { CStr::from_ptr(url) }.to_str() else {
        error!(target: LOG_TARGET, "OTA URL is not valid UTF-8");
        return false;
    };

    match install(url) {
        Ok(()) => true,
        Err(error) => {
            error!(target: LOG_TARGET, "OTA failed: {error}");
            false
        }
    }
}
