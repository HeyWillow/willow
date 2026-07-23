//! Pure HTTP audio format selection from headers and URLs.

use core::fmt;

const ESP_HTTP_DEFAULT_TRANSMIT_BUFFER_BYTES: usize = 512;
const ESP_HTTP_MAX_URL_BYTES: usize = 4 * 1024;
const HTTP_GET_REQUEST_LINE_FIXED_BYTES: usize = b"GET  HTTP/1.1\r\n\0".len();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HttpAudioFormat {
    Aac,
    AmrNb,
    AmrWb,
    Flac,
    M4a,
    Mp3,
    Ogg,
    Opus,
    Pcm,
    TransportStream,
    Wav,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct HttpAudioFormatError {
    url: String,
    content_type: Option<String>,
}

impl fmt::Display for HttpAudioFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot select an audio codec for URL {:?}",
            self.url
        )?;
        if let Some(content_type) = &self.content_type {
            write!(formatter, " with Content-Type {content_type:?}")?;
        }
        Ok(())
    }
}

impl std::error::Error for HttpAudioFormatError {}

/// Sizes ESP HTTP's transmit buffer to hold the GET request line and headers.
pub(crate) const fn transmit_buffer_size(url_bytes: usize) -> Option<usize> {
    if url_bytes > ESP_HTTP_MAX_URL_BYTES {
        return None;
    }

    let Some(request_bytes) = url_bytes.checked_add(HTTP_GET_REQUEST_LINE_FIXED_BYTES) else {
        return None;
    };
    // ESP-IDF generates the first request line and as many complete headers as
    // fit in the same buffer. Leave its normal buffer size available after a
    // long request target instead of consuming every byte with the URL.
    request_bytes.checked_add(ESP_HTTP_DEFAULT_TRANSMIT_BUFFER_BYTES)
}

pub(crate) fn select(
    url: &str,
    content_type: Option<&str>,
) -> Result<HttpAudioFormat, HttpAudioFormatError> {
    content_type
        .and_then(from_content_type)
        .or_else(|| from_url(url))
        .ok_or_else(|| HttpAudioFormatError {
            url: url.to_owned(),
            content_type: content_type.map(str::to_owned),
        })
}

fn from_content_type(content_type: &str) -> Option<HttpAudioFormat> {
    let media_type = content_type.split(';').next()?.trim();
    if media_type.eq_ignore_ascii_case("audio/aac") || media_type.eq_ignore_ascii_case("audio/aacp")
    {
        Some(HttpAudioFormat::Aac)
    } else if media_type.eq_ignore_ascii_case("audio/amr") {
        Some(HttpAudioFormat::AmrNb)
    } else if media_type.eq_ignore_ascii_case("audio/amr-wb") {
        Some(HttpAudioFormat::AmrWb)
    } else if media_type.eq_ignore_ascii_case("audio/flac")
        || media_type.eq_ignore_ascii_case("audio/x-flac")
    {
        Some(HttpAudioFormat::Flac)
    } else if media_type.eq_ignore_ascii_case("audio/mp4")
        || media_type.eq_ignore_ascii_case("audio/m4a")
        || media_type.eq_ignore_ascii_case("audio/x-m4a")
    {
        Some(HttpAudioFormat::M4a)
    } else if media_type.eq_ignore_ascii_case("audio/mpeg")
        || media_type.eq_ignore_ascii_case("audio/mp3")
    {
        Some(HttpAudioFormat::Mp3)
    } else if media_type.eq_ignore_ascii_case("audio/ogg")
        || media_type.eq_ignore_ascii_case("application/ogg")
        || media_type.eq_ignore_ascii_case("audio/vorbis")
        || media_type.eq_ignore_ascii_case("audio/x-vorbis+ogg")
    {
        Some(HttpAudioFormat::Ogg)
    } else if media_type.eq_ignore_ascii_case("audio/opus") {
        Some(HttpAudioFormat::Opus)
    } else if media_type.eq_ignore_ascii_case("audio/pcm")
        || media_type.eq_ignore_ascii_case("audio/x-raw")
    {
        Some(HttpAudioFormat::Pcm)
    } else if media_type.eq_ignore_ascii_case("video/mp2t") {
        Some(HttpAudioFormat::TransportStream)
    } else if media_type.eq_ignore_ascii_case("audio/wav")
        || media_type.eq_ignore_ascii_case("audio/wave")
        || media_type.eq_ignore_ascii_case("audio/x-wav")
        || media_type.eq_ignore_ascii_case("audio/vnd.wave")
    {
        Some(HttpAudioFormat::Wav)
    } else {
        None
    }
}

fn from_url(url: &str) -> Option<HttpAudioFormat> {
    let path = url.split(['?', '#']).next()?;
    let extension = path.rsplit_once('.')?.1;
    if extension.eq_ignore_ascii_case("aac") {
        Some(HttpAudioFormat::Aac)
    } else if extension.eq_ignore_ascii_case("amr") || extension.eq_ignore_ascii_case("amrnb") {
        Some(HttpAudioFormat::AmrNb)
    } else if extension.eq_ignore_ascii_case("amrwb") || extension.eq_ignore_ascii_case("awb") {
        Some(HttpAudioFormat::AmrWb)
    } else if extension.eq_ignore_ascii_case("flac") {
        Some(HttpAudioFormat::Flac)
    } else if extension.eq_ignore_ascii_case("m4a") || extension.eq_ignore_ascii_case("mp4") {
        Some(HttpAudioFormat::M4a)
    } else if extension.eq_ignore_ascii_case("mp3") {
        Some(HttpAudioFormat::Mp3)
    } else if extension.eq_ignore_ascii_case("ogg") || extension.eq_ignore_ascii_case("oga") {
        Some(HttpAudioFormat::Ogg)
    } else if extension.eq_ignore_ascii_case("opus") {
        Some(HttpAudioFormat::Opus)
    } else if extension.eq_ignore_ascii_case("pcm") || extension.eq_ignore_ascii_case("raw") {
        Some(HttpAudioFormat::Pcm)
    } else if extension.eq_ignore_ascii_case("ts") {
        Some(HttpAudioFormat::TransportStream)
    } else if extension.eq_ignore_ascii_case("wav") {
        Some(HttpAudioFormat::Wav)
    } else {
        None
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the firmware binary disables Cargo's test harness"
)]
mod tests {
    #[test]
    fn reserves_header_space_after_the_request_line() {
        assert_eq!(super::transmit_buffer_size(100), Some(628));
    }

    #[test]
    fn expands_the_transmit_buffer_for_long_urls() {
        assert_eq!(super::transmit_buffer_size(800), Some(1_328));
    }

    #[test]
    fn rejects_transmit_buffer_sizes_that_esp_http_cannot_represent() {
        assert_eq!(super::transmit_buffer_size(usize::MAX), None);
        assert_eq!(super::transmit_buffer_size(4_097), None);
    }

    #[test]
    fn prefers_recognized_content_type() {
        assert_eq!(
            super::select(
                "https://example.test/speech.mp3",
                Some("audio/wav; charset=binary")
            ),
            Ok(super::HttpAudioFormat::Wav)
        );
    }

    #[test]
    fn falls_back_to_url_extension() {
        assert_eq!(
            super::select(
                "https://example.test/speech.MP3?token=abc",
                Some("application/octet-stream")
            ),
            Ok(super::HttpAudioFormat::Mp3)
        );
    }

    #[test]
    fn maps_all_supported_media_types() {
        for (content_type, expected) in [
            ("audio/aac", super::HttpAudioFormat::Aac),
            ("audio/amr", super::HttpAudioFormat::AmrNb),
            ("audio/amr-wb", super::HttpAudioFormat::AmrWb),
            ("audio/flac", super::HttpAudioFormat::Flac),
            ("audio/mp4", super::HttpAudioFormat::M4a),
            ("audio/mpeg", super::HttpAudioFormat::Mp3),
            ("audio/ogg", super::HttpAudioFormat::Ogg),
            ("audio/opus", super::HttpAudioFormat::Opus),
            ("audio/pcm", super::HttpAudioFormat::Pcm),
            ("video/mp2t", super::HttpAudioFormat::TransportStream),
            ("audio/x-wav", super::HttpAudioFormat::Wav),
        ] {
            assert_eq!(
                super::select("https://example.test/audio", Some(content_type)),
                Ok(expected)
            );
        }
    }

    #[test]
    fn maps_container_and_raw_extensions() {
        for (extension, expected) in [
            ("ogg", super::HttpAudioFormat::Ogg),
            ("oga", super::HttpAudioFormat::Ogg),
            ("opus", super::HttpAudioFormat::Opus),
            ("pcm", super::HttpAudioFormat::Pcm),
            ("raw", super::HttpAudioFormat::Pcm),
        ] {
            let url = format!("https://example.test/speech.{extension}");
            assert_eq!(super::select(&url, None), Ok(expected));
        }
    }

    #[test]
    fn reports_unknown_format_with_inputs() {
        let error = super::select(
            "https://example.test/speech",
            Some("application/octet-stream"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("application/octet-stream"));
        assert!(error.to_string().contains("https://example.test/speech"));
    }
}
