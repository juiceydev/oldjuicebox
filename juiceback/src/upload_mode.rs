/// Upload transport mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UploadMode {
    /// Standard HTTP upload via axum.
    Standard,
    /// QUIC/HTTP3 upload via h3.
    Quic,
}

impl From<&str> for UploadMode {
    fn from(s: &str) -> Self {
        match s {
            "quic" => UploadMode::Quic,
            _ => UploadMode::Standard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_quic() {
        assert_eq!(UploadMode::from("quic"), UploadMode::Quic);
    }

    #[test]
    fn from_standard() {
        assert_eq!(UploadMode::from("standard"), UploadMode::Standard);
    }

    #[test]
    fn from_empty() {
        assert_eq!(UploadMode::from(""), UploadMode::Standard);
    }

    #[test]
    fn from_random_string() {
        assert_eq!(UploadMode::from("anything"), UploadMode::Standard);
    }
}
