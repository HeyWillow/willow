//! Bounded chunked HTTP upload for WIS audio sessions.

use core::{fmt, time::Duration};
use std::{collections::TryReserveError, io};

use esp_idf_svc::{
    handle::RawHandle,
    http::client::{Configuration, EspHttpConnection, Method},
};
use esp_idf_sys::{
    ESP_ERR_HTTP_EAGAIN, ESP_ERR_TIMEOUT, EspError,
    esp_http_client_auth_type_t_HTTP_AUTH_TYPE_BASIC, esp_http_client_is_complete_data_received,
    esp_http_client_set_authtype,
};

use super::{
    capture,
    http_chunk::ChunkedBodyWriter,
    wis_encoder::{EncodingFinish, WisEncoder, WisEncodingError},
    wis_framing::WisFormat,
};

const AUDIO_BITS_HEADER: &str = "16";
const AUDIO_CHANNEL_HEADER: &str = "1";
const AUDIO_SAMPLE_RATE_HEADER: &str = "16000";
const HTTP_OK: u16 = 200;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 2_047;
const RESPONSE_ALLOCATION_BYTES: usize = MAX_RESPONSE_BYTES + 1;
const USER_AGENT: &str = concat!("Willow/", env!("WILLOW_VERSION"));

const _: () = assert!(capture::SAMPLE_RATE_HZ == 16_000);

#[derive(Debug)]
pub(super) enum WisUploadError {
    Cancelled {
        url: String,
    },
    Encoding {
        url: String,
        source: WisEncodingError,
    },
    Http {
        url: String,
        operation: &'static str,
        source: EspError,
    },
    ChunkWrite {
        url: String,
        source: io::Error,
    },
    HttpStatus {
        url: String,
        status: u16,
    },
    AllocateResponse {
        url: String,
        bytes: usize,
        source: TryReserveError,
    },
    ResponseTooLarge {
        url: String,
        limit: usize,
    },
    EmptyResponse {
        url: String,
    },
    IncompleteResponse {
        url: String,
        bytes: usize,
    },
}

impl WisUploadError {
    fn http<'url>(url: &'url str, operation: &'static str) -> impl FnOnce(EspError) -> Self + 'url {
        move |source| Self::Http {
            url: url.to_owned(),
            operation,
            source,
        }
    }
}

impl fmt::Display for WisUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled { url } => write!(formatter, "WIS upload to {url:?} was cancelled"),
            Self::Encoding { url, source } => {
                write!(
                    formatter,
                    "failed to encode WIS audio for {url:?}: {source}"
                )
            }
            Self::Http {
                url,
                operation,
                source,
            } => write!(
                formatter,
                "failed to {operation} for WIS URL {url:?}: {source}"
            ),
            Self::ChunkWrite { url, source } => {
                write!(formatter, "failed to finish WIS body for {url:?}: {source}")
            }
            Self::HttpStatus { url, status } => {
                write!(formatter, "WIS URL {url:?} returned HTTP {status}")
            }
            Self::AllocateResponse { url, bytes, source } => write!(
                formatter,
                "failed to allocate {bytes} bytes for the WIS response from {url:?}: {source}"
            ),
            Self::ResponseTooLarge { url, limit } => write!(
                formatter,
                "WIS response from {url:?} exceeds the {limit}-byte limit"
            ),
            Self::EmptyResponse { url } => {
                write!(formatter, "WIS URL {url:?} returned an empty response")
            }
            Self::IncompleteResponse { url, bytes } => write!(
                formatter,
                "WIS response from {url:?} ended after {bytes} bytes without completing"
            ),
        }
    }
}

impl std::error::Error for WisUploadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encoding { source, .. } => Some(source),
            Self::Http { source, .. } => Some(source),
            Self::ChunkWrite { source, .. } => Some(source),
            Self::AllocateResponse { source, .. } => Some(source),
            Self::Cancelled { .. }
            | Self::HttpStatus { .. }
            | Self::ResponseTooLarge { .. }
            | Self::EmptyResponse { .. }
            | Self::IncompleteResponse { .. } => None,
        }
    }
}

/// Successful WIS response and encoder-close metadata.
pub(super) struct WisUploadResponse {
    body: Vec<u8>,
    pub(super) encoding_finish: EncodingFinish,
}

impl WisUploadResponse {
    pub(super) fn into_body(self) -> Vec<u8> {
        self.body
    }
}

/// Owns one streaming HTTP request and its per-request encoder.
pub(super) struct WisUpload {
    url: String,
    connection: EspHttpConnection,
    encoder: WisEncoder,
}

impl WisUpload {
    pub(super) fn start(
        url: &str,
        format: WisFormat,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, WisUploadError> {
        ensure_not_cancelled(url, cancelled)?;
        let encoder = WisEncoder::new(format).map_err(|source| WisUploadError::Encoding {
            url: url.to_owned(),
            source,
        })?;
        let configuration = Configuration {
            timeout: Some(HTTP_TIMEOUT),
            // The pure writer below owns chunk framing so it can be tested and
            // every partial native write can be completed explicitly.
            raw_request_body: true,
            ..Default::default()
        };
        let mut connection = EspHttpConnection::new(&configuration)
            .map_err(WisUploadError::http(url, "initialize HTTP client"))?;

        // Preserve the old HTTP stream callback's explicit Basic auth mode.
        // ESP-IDF obtains credentials from the URL when supplied there.
        let auth_status = unsafe {
            esp_http_client_set_authtype(
                connection.handle(),
                esp_http_client_auth_type_t_HTTP_AUTH_TYPE_BASIC,
            )
        };
        if let Some(source) = EspError::from(auth_status) {
            return Err(WisUploadError::http(url, "configure HTTP Basic auth")(
                source,
            ));
        }

        connection
            .initiate_request(
                Method::Post,
                url,
                &[
                    ("User-Agent", USER_AGENT),
                    ("x-audio-sample-rate", AUDIO_SAMPLE_RATE_HEADER),
                    ("x-audio-bits", AUDIO_BITS_HEADER),
                    ("x-audio-channel", AUDIO_CHANNEL_HEADER),
                    ("x-audio-codec", format.header_value()),
                ],
            )
            .map_err(WisUploadError::http(url, "open chunked HTTP request"))?;
        ensure_not_cancelled(url, cancelled)?;

        Ok(Self {
            url: url.to_owned(),
            connection,
            encoder,
        })
    }

    pub(super) fn write_samples(
        &mut self,
        samples: &[i16],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), WisUploadError> {
        ensure_not_cancelled(&self.url, cancelled)?;
        let mut raw = RawRequestWriter {
            connection: &mut self.connection,
            cancelled,
        };
        let mut chunks = ChunkedBodyWriter::new(&mut raw);
        let result = self.encoder.write_samples(samples, &mut chunks);
        if cancelled() {
            return Err(WisUploadError::Cancelled {
                url: self.url.clone(),
            });
        }
        result.map_err(|source| WisUploadError::Encoding {
            url: self.url.clone(),
            source,
        })
    }

    pub(super) fn finish(
        mut self,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<WisUploadResponse, WisUploadError> {
        ensure_not_cancelled(&self.url, cancelled)?;
        let encoding_finish = self.encoder.finish();
        {
            let mut raw = RawRequestWriter {
                connection: &mut self.connection,
                cancelled,
            };
            ChunkedBodyWriter::new(&mut raw)
                .finish()
                .map_err(|source| WisUploadError::ChunkWrite {
                    url: self.url.clone(),
                    source,
                })?;
        }
        ensure_not_cancelled(&self.url, cancelled)?;
        self.connection
            .initiate_response()
            .map_err(WisUploadError::http(
                &self.url,
                "read HTTP response headers",
            ))?;

        let status = self.connection.status();
        if status != HTTP_OK {
            return Err(WisUploadError::HttpStatus {
                url: self.url,
                status,
            });
        }
        let body = read_response(&mut self.connection, &self.url, cancelled)?;
        Ok(WisUploadResponse {
            body,
            encoding_finish,
        })
    }
}

struct RawRequestWriter<'connection, 'cancelled> {
    connection: &'connection mut EspHttpConnection,
    cancelled: &'cancelled dyn Fn() -> bool,
}

impl io::Write for RawRequestWriter<'_, '_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "WIS upload was cancelled",
            ));
        }
        let written = self.connection.write(buffer).map_err(http_io_error)?;
        if written > buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "HTTP client reported {written} bytes for a {}-byte write",
                    buffer.len()
                ),
            ));
        }
        if written == 0 && !buffer.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "HTTP client accepted no request bytes",
            ));
        }
        if (self.cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "WIS upload was cancelled",
            ));
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_response(
    connection: &mut EspHttpConnection,
    url: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, WisUploadError> {
    let mut body = allocate_response(url)?;
    let mut bytes = 0;
    loop {
        ensure_not_cancelled(url, cancelled)?;
        if bytes == body.len() {
            return Err(WisUploadError::ResponseTooLarge {
                url: url.to_owned(),
                limit: MAX_RESPONSE_BYTES,
            });
        }
        match connection.read(&mut body[bytes..]) {
            Ok(0) => break,
            Ok(read) => {
                bytes += read;
                if bytes > MAX_RESPONSE_BYTES {
                    return Err(WisUploadError::ResponseTooLarge {
                        url: url.to_owned(),
                        limit: MAX_RESPONSE_BYTES,
                    });
                }
            }
            Err(source) if source.code() == ESP_ERR_HTTP_EAGAIN => {}
            Err(source) => {
                return Err(WisUploadError::Http {
                    url: url.to_owned(),
                    operation: "read HTTP response body",
                    source,
                });
            }
        }
    }
    if bytes == 0 {
        return Err(WisUploadError::EmptyResponse {
            url: url.to_owned(),
        });
    }
    if !unsafe { esp_http_client_is_complete_data_received(connection.handle()) } {
        return Err(WisUploadError::IncompleteResponse {
            url: url.to_owned(),
            bytes,
        });
    }
    body.truncate(bytes);
    Ok(body)
}

fn allocate_response(url: &str) -> Result<Vec<u8>, WisUploadError> {
    let mut body = Vec::new();
    body.try_reserve_exact(RESPONSE_ALLOCATION_BYTES)
        .map_err(|source| WisUploadError::AllocateResponse {
            url: url.to_owned(),
            bytes: RESPONSE_ALLOCATION_BYTES,
            source,
        })?;
    body.resize(RESPONSE_ALLOCATION_BYTES, 0);
    Ok(body)
}

fn ensure_not_cancelled(url: &str, cancelled: &dyn Fn() -> bool) -> Result<(), WisUploadError> {
    if cancelled() {
        Err(WisUploadError::Cancelled {
            url: url.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn http_io_error(source: EspError) -> io::Error {
    let kind = match source.code() {
        ESP_ERR_TIMEOUT => io::ErrorKind::TimedOut,
        ESP_ERR_HTTP_EAGAIN => io::ErrorKind::WouldBlock,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, source)
}
