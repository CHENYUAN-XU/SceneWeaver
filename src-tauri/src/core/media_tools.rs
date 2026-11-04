use std::path::PathBuf;

#[derive(Clone, Copy)]
pub(super) enum MediaTool {
    Ffmpeg,
    Ffprobe,
}

impl MediaTool {
    pub(super) fn find(self) -> Option<PathBuf> {
        let name = match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
        };
        let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        if let Some(bundled) = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .map(|parent| parent.join("media-tools").join(&executable))
            })
            .filter(|file| file.is_file())
        {
            return Some(bundled);
        }
        which::which(name).ok().or_else(|| {
            ["ProgramFiles", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(|root| {
                    PathBuf::from(root)
                        .join("FFmpeg")
                        .join("bin")
                        .join(&executable)
                })
                .find(|candidate| candidate.is_file())
        })
    }
}
