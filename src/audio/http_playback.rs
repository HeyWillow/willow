//! HTTP(S) streaming source for the bounded playback pump.

use core::{fmt, time::Duration};
use std::{io, time::Instant};

use esp_idf_svc::{
    handle::RawHandle,
    http::client::{Configuration, EspHttpConnection, Method},
};
use esp_idf_sys::{
    EspError, esp_http_client_auth_type_t_HTTP_AUTH_TYPE_BASIC,
    esp_http_client_is_complete_data_received, esp_http_client_set_authtype,
    esp_http_client_set_timeout_ms,
};

use super::{
    http_audio::{self, HttpAudioFormat, HttpAudioFormatError},
    i2s::TransmitChannel,
    playback::{self, PlaybackError, PlaybackWorkspace},
    stream_codec::{CodecLibrary, StreamFormat},
};

const HTTP_OK: u16 = 200;
const HTTP_PARTIAL_CONTENT: u16 = 206;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const READ_POLL_TIMEOUT_MS: i32 = 100;

#[derive(Debug)]
pub(super) enum HttpPlaybackError {
    Esp {
        url: String,
        operation: &'static str,
        source: EspError,
    },
    HttpStatus {
        url: String,
        status: u16,
    },
    Format {
        source: HttpAudioFormatError,
    },
    Playback {
        url: String,
        source: PlaybackError,
    },
    IncompleteResponse {
        url: String,
    },
    UrlTooLong {
        bytes: usize,
    },
}

impl HttpPlaybackError {
    fn esp<'url>(url: &'url str, operation: &'static str) -> impl FnOnce(EspError) -> Self + 'url {
        move |source| Self::Esp {
            url: url.to_owned(),
            operation,
            source,
        }
    }
}

impl fmt::Display for HttpPlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Esp {
                url,
                operation,
                source,
            } => write!(
                formatter,
                "failed to {operation} for audio URL {url:?}: {source}"
            ),
            Self::HttpStatus { url, status } => {
                write!(formatter, "audio URL {url:?} returned HTTP {status}")
            }
            Self::Format { source } => write!(formatter, "HTTP audio format failed: {source}"),
            Self::Playback { url, source } => {
                write!(formatter, "failed to play HTTP audio URL {url:?}: {source}")
            }
            Self::IncompleteResponse { url } => {
                write!(
                    formatter,
                    "audio URL {url:?} ended before its HTTP body completed"
                )
            }
            Self::UrlTooLong { bytes } => write!(
                formatter,
                "{bytes}-byte audio URL is too long for the ESP HTTP client"
            ),
        }
    }
}

impl std::error::Error for HttpPlaybackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Esp { source, .. } => Some(source),
            Self::Format { source } => Some(source),
            Self::Playback { source, .. } => Some(source),
            Self::HttpStatus { .. } | Self::IncompleteResponse { .. } | Self::UrlTooLong { .. } => {
                None
            }
        }
    }
}

impl HttpPlaybackError {
    pub(super) const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Playback { source, .. } if source.is_cancelled())
    }
}

pub(super) fn play(
    url: &str,
    codecs: &CodecLibrary,
    transmit: &mut TransmitChannel,
    workspace: &mut PlaybackWorkspace<'_>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), HttpPlaybackError> {
    let buffer_size_tx = http_audio::transmit_buffer_size(url.len())
        .ok_or(HttpPlaybackError::UrlTooLong { bytes: url.len() })?;
    let configuration = Configuration {
        buffer_size_tx: Some(buffer_size_tx),
        timeout: Some(HTTP_TIMEOUT),
        ..Default::default()
    };
    let mut connection = EspHttpConnection::new(&configuration)
        .map_err(HttpPlaybackError::esp(url, "initialize HTTP client"))?;

    // Preserve the old HTTP stream callback's explicit Basic auth mode. The
    // ESP-IDF client obtains credentials from the URL when supplied there.
    let auth_status = unsafe {
        esp_http_client_set_authtype(
            connection.handle(),
            esp_http_client_auth_type_t_HTTP_AUTH_TYPE_BASIC,
        )
    };
    if let Some(source) = EspError::from(auth_status) {
        return Err(HttpPlaybackError::esp(url, "configure HTTP Basic auth")(
            source,
        ));
    }

    connection
        .initiate_request(Method::Get, url, &[])
        .map_err(HttpPlaybackError::esp(url, "open HTTP request"))?;
    connection
        .initiate_response()
        .map_err(HttpPlaybackError::esp(url, "read HTTP response headers"))?;

    let status = connection.status();
    if !matches!(status, HTTP_OK | HTTP_PARTIAL_CONTENT) {
        return Err(HttpPlaybackError::HttpStatus {
            url: url.to_owned(),
            status,
        });
    }
    let format = http_audio::select(url, connection.header("Content-Type"))
        .map(StreamFormat::from)
        .map_err(|source| HttpPlaybackError::Format { source })?;
    let timeout_status =
        unsafe { esp_http_client_set_timeout_ms(connection.handle(), READ_POLL_TIMEOUT_MS) };
    if let Some(source) = EspError::from(timeout_status) {
        return Err(HttpPlaybackError::esp(url, "configure audio read polling")(
            source,
        ));
    }

    playback::play_reader(
        &mut HttpReader::new(&mut connection),
        format,
        codecs,
        transmit,
        workspace,
        cancelled,
    )
    .map_err(|source| HttpPlaybackError::Playback {
        url: url.to_owned(),
        source,
    })?;

    if !unsafe { esp_http_client_is_complete_data_received(connection.handle()) } {
        return Err(HttpPlaybackError::IncompleteResponse {
            url: url.to_owned(),
        });
    }
    Ok(())
}

struct HttpReader<'connection> {
    connection: &'connection mut EspHttpConnection,
    last_progress: Instant,
}

impl<'connection> HttpReader<'connection> {
    fn new(connection: &'connection mut EspHttpConnection) -> Self {
        Self {
            connection,
            last_progress: Instant::now(),
        }
    }
}

impl io::Read for HttpReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.connection.read(buffer) {
            Ok(bytes) => {
                if bytes > 0 {
                    self.last_progress = Instant::now();
                }
                Ok(bytes)
            }
            Err(source)
                if source.code() == super::HTTP_EAGAIN
                    && self.last_progress.elapsed() < HTTP_TIMEOUT =>
            {
                Err(io::Error::new(io::ErrorKind::Interrupted, source))
            }
            Err(source) if source.code() == super::HTTP_EAGAIN => {
                Err(io::Error::new(io::ErrorKind::TimedOut, source))
            }
            Err(source) => Err(io::Error::other(source)),
        }
    }
}

impl From<HttpAudioFormat> for StreamFormat {
    fn from(format: HttpAudioFormat) -> Self {
        match format {
            HttpAudioFormat::Aac => Self::Aac,
            HttpAudioFormat::AmrNb => Self::AmrNb,
            HttpAudioFormat::AmrWb => Self::AmrWb,
            HttpAudioFormat::Flac => Self::Flac,
            HttpAudioFormat::M4a => Self::M4a,
            HttpAudioFormat::Mp3 => Self::Mp3,
            HttpAudioFormat::Ogg => Self::Ogg,
            HttpAudioFormat::Opus => Self::Opus,
            HttpAudioFormat::Pcm => Self::Pcm,
            HttpAudioFormat::TransportStream => Self::TransportStream,
            HttpAudioFormat::Wav => Self::Wav,
        }
    }
}
