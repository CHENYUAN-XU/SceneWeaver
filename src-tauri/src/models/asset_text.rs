use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetTextSource {
    Ocr,
    Speech,
}

impl AssetTextSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::Speech => "speech",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TextCue {
    pub text: String,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}
