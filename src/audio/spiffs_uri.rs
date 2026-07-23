//! Pure SPIFFS audio URI normalization and format selection.

use core::fmt;
use std::path::{Component, Path, PathBuf};

const URI_SCHEME: &str = "spiffs://";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AudioFileFormat {
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
pub(crate) enum SpiffsUriError {
    InvalidUri {
        uri: String,
    },
    OutsideMount {
        uri: String,
        mount: PathBuf,
        path: PathBuf,
    },
    MissingExtension {
        uri: String,
        path: PathBuf,
    },
    UnsupportedExtension {
        uri: String,
        extension: String,
    },
}

impl fmt::Display for SpiffsUriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUri { uri } => write!(formatter, "invalid SPIFFS audio URI {uri:?}"),
            Self::OutsideMount { uri, mount, path } => write!(
                formatter,
                "SPIFFS audio URI {uri:?} resolves outside {} as {}",
                mount.display(),
                path.display()
            ),
            Self::MissingExtension { uri, path } => write!(
                formatter,
                "SPIFFS audio URI {uri:?} has no codec extension in {}",
                path.display()
            ),
            Self::UnsupportedExtension { uri, extension } => write!(
                formatter,
                "SPIFFS audio URI {uri:?} uses unsupported extension {extension:?}"
            ),
        }
    }
}

impl std::error::Error for SpiffsUriError {}

pub(crate) fn resolve(uri: &str, mount: &Path) -> Result<PathBuf, SpiffsUriError> {
    let relative = uri
        .strip_prefix(URI_SCHEME)
        .ok_or_else(|| SpiffsUriError::InvalidUri {
            uri: uri.to_owned(),
        })?;
    if relative.is_empty() {
        return Err(SpiffsUriError::InvalidUri {
            uri: uri.to_owned(),
        });
    }

    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(SpiffsUriError::InvalidUri {
            uri: uri.to_owned(),
        });
    }

    let path = Path::new("/").join(relative);
    if path == mount || !path.starts_with(mount) {
        return Err(SpiffsUriError::OutsideMount {
            uri: uri.to_owned(),
            mount: mount.to_owned(),
            path,
        });
    }
    Ok(path)
}

pub(crate) fn format(uri: &str, path: &Path) -> Result<AudioFileFormat, SpiffsUriError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .ok_or_else(|| SpiffsUriError::MissingExtension {
            uri: uri.to_owned(),
            path: path.to_owned(),
        })?;

    if extension.eq_ignore_ascii_case("aac") {
        Ok(AudioFileFormat::Aac)
    } else if extension.eq_ignore_ascii_case("amr") || extension.eq_ignore_ascii_case("amrnb") {
        Ok(AudioFileFormat::AmrNb)
    } else if extension.eq_ignore_ascii_case("amrwb") || extension.eq_ignore_ascii_case("awb") {
        Ok(AudioFileFormat::AmrWb)
    } else if extension.eq_ignore_ascii_case("flac") {
        Ok(AudioFileFormat::Flac)
    } else if extension.eq_ignore_ascii_case("m4a") || extension.eq_ignore_ascii_case("mp4") {
        Ok(AudioFileFormat::M4a)
    } else if extension.eq_ignore_ascii_case("mp3") {
        Ok(AudioFileFormat::Mp3)
    } else if extension.eq_ignore_ascii_case("ogg") || extension.eq_ignore_ascii_case("oga") {
        Ok(AudioFileFormat::Ogg)
    } else if extension.eq_ignore_ascii_case("opus") {
        Ok(AudioFileFormat::Opus)
    } else if extension.eq_ignore_ascii_case("pcm") || extension.eq_ignore_ascii_case("raw") {
        Ok(AudioFileFormat::Pcm)
    } else if extension.eq_ignore_ascii_case("ts") {
        Ok(AudioFileFormat::TransportStream)
    } else if extension.eq_ignore_ascii_case("wav") {
        Ok(AudioFileFormat::Wav)
    } else {
        Err(SpiffsUriError::UnsupportedExtension {
            uri: uri.to_owned(),
            extension: extension.to_owned(),
        })
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the firmware binary disables Cargo's test harness"
)]
mod tests {
    #[test]
    fn resolves_existing_willow_uri_shape() {
        assert_eq!(
            super::resolve(
                "spiffs://spiffs/user/audio/success.wav",
                std::path::Path::new("/spiffs/user")
            ),
            Ok(std::path::PathBuf::from("/spiffs/user/audio/success.wav"))
        );
    }

    #[test]
    fn rejects_escape_and_other_partitions() {
        for uri in [
            "spiffs://spiffs/user/../secret.wav",
            "spiffs://spiffs/other/audio.wav",
            "file:///spiffs/user/audio.wav",
        ] {
            assert!(
                super::resolve(uri, std::path::Path::new("/spiffs/user")).is_err(),
                "accepted {uri}"
            );
        }
    }

    #[test]
    fn maps_extensions_without_case_sensitivity() {
        for (extension, expected) in [
            ("AAC", super::AudioFileFormat::Aac),
            ("amr", super::AudioFileFormat::AmrNb),
            ("amrnb", super::AudioFileFormat::AmrNb),
            ("amrwb", super::AudioFileFormat::AmrWb),
            ("awb", super::AudioFileFormat::AmrWb),
            ("flac", super::AudioFileFormat::Flac),
            ("m4a", super::AudioFileFormat::M4a),
            ("mp4", super::AudioFileFormat::M4a),
            ("mp3", super::AudioFileFormat::Mp3),
            ("ogg", super::AudioFileFormat::Ogg),
            ("oga", super::AudioFileFormat::Ogg),
            ("opus", super::AudioFileFormat::Opus),
            ("pcm", super::AudioFileFormat::Pcm),
            ("raw", super::AudioFileFormat::Pcm),
            ("ts", super::AudioFileFormat::TransportStream),
            ("WAV", super::AudioFileFormat::Wav),
        ] {
            let uri = format!("spiffs://spiffs/user/audio.{extension}");
            let path = std::path::PathBuf::from(format!("/spiffs/user/audio.{extension}"));
            assert_eq!(super::format(&uri, &path), Ok(expected));
        }
    }
}
