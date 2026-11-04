use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::core::cache::CacheManager;
use crate::core::error::{AppError, AppResult};
use crate::core::ffprobe::run_command_with_timeout;
use crate::core::media_tools::MediaTool;
use crate::models::{Asset, MediaType, Segment, TextCue};

pub fn recognize_text(
    asset: &Asset,
    segments: &[Segment],
    cache: &CacheManager,
    language: &str,
) -> AppResult<Vec<TextCue>> {
    if !matches!(language, "eng" | "chi_sim" | "jpn" | "eng+chi_sim") {
        return Err(AppError::Other("请选择支持的 OCR 语言".into()));
    }
    let mut command = local_tool("tesseract", "未找到 Tesseract，请安装并将其加入 PATH")?;
    let executable = command.get_program().to_owned();
    let mut sources = Vec::new();
    match asset.media_type {
        MediaType::Image => sources.push((Path::new(&asset.file_path).to_path_buf(), None, None)),
        MediaType::Video => {
            let available = segments
                .iter()
                .filter_map(|segment| {
                    let path = cache.keyframe_path(&asset.id, segment.segment_index);
                    (segment.representative_frame_path.is_some() && path.is_file()).then_some((
                        path,
                        Some(segment.start_ms),
                        Some(segment.end_ms),
                    ))
                })
                .collect::<Vec<_>>();
            let step = available.len().div_ceil(24).max(1);
            sources.extend(available.into_iter().step_by(step));
            if sources.is_empty() {
                return Err(AppError::Other("视频尚无可用关键帧，请先切分镜头".into()));
            }
        }
        _ => return Err(AppError::Other("只有图片或视频可以识别画面文字".into())),
    }
    let mut cues = Vec::new();
    for (path, start_ms, end_ms) in sources {
        command = Command::new(&executable);
        command.arg(path).args(["stdout", "-l", language]);
        let result = run_text_tool(&mut command, Duration::from_secs(30), "画面文字识别失败")?;
        let text = result.trim();
        if !text.is_empty() {
            cues.push(TextCue {
                text: text.to_string(),
                start_ms,
                end_ms,
            });
        }
    }
    Ok(cues)
}

pub fn transcribe(asset: &Asset, model: &Path) -> AppResult<Vec<TextCue>> {
    if !matches!(asset.media_type, MediaType::Video | MediaType::Audio) {
        return Err(AppError::Other("只有音频或视频可以转写语音".into()));
    }
    if !model.is_absolute() || !model.is_file() {
        return Err(AppError::InvalidPath(model.to_path_buf()));
    }
    let executable = MediaTool::Ffmpeg
        .find()
        .ok_or_else(|| AppError::Other("未找到 FFmpeg，请安装含 whisper 滤镜的版本".into()))?;
    let mut command = Command::new(executable);
    let model = escape_filter_option(&model.to_string_lossy().replace('\\', "/"));
    let destination = escape_filter_option("pipe:1");
    let filter = format!("aformat=sample_rates=16000:channel_layouts=mono,whisper=model={model}:destination={destination}:format=srt:use_gpu=false:queue=15");
    command
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-i"])
        .arg(&asset.file_path)
        .args(["-map", "0:a:0", "-af", &filter, "-f", "null", "-"]);
    let text = run_text_tool(&mut command, Duration::from_secs(15 * 60), "语音转写失败")?;
    Ok(parse_srt(&text))
}

fn local_tool(name: &str, missing: &str) -> AppResult<Command> {
    let executable = which::which(name).map_err(|_| AppError::Other(missing.into()))?;
    Ok(Command::new(executable))
}

fn run_text_tool(command: &mut Command, timeout: Duration, label: &str) -> AppResult<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let result = run_command_with_timeout(command, timeout, "本地文字分析超时，已停止子进程")?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let detail = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("本地工具异常退出");
        return Err(AppError::Other(format!("{label}：{detail}")));
    }
    Ok(String::from_utf8_lossy(&result.stdout).into_owned())
}

// Filter options and the filtergraph each interpret their own escapes.
fn escape_filter_option(value: &str) -> String {
    fn escape(value: &str, special: &str) -> String {
        let mut escaped = String::new();
        for character in value.chars() {
            if special.contains(character) {
                escaped.push('\\');
            }
            escaped.push(character);
        }
        escaped
    }
    escape(&escape(value, "\\':"), "\\'[],;")
}

pub fn parse_srt(input: &str) -> Vec<TextCue> {
    input
        .replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(|block| {
            let mut lines = block.lines();
            let (start, end) = lines.find_map(|line| line.split_once("-->"))?;
            let start_ms = parse_timestamp(start.trim())?;
            let end_ms = parse_timestamp(end.split_whitespace().next()?)?;
            let text = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
            (end_ms > start_ms && !text.is_empty()).then_some(TextCue {
                text,
                start_ms: Some(start_ms),
                end_ms: Some(end_ms),
            })
        })
        .collect()
}

fn parse_timestamp(value: &str) -> Option<i64> {
    let (hours, rest) = value.split_once(':')?;
    let (minutes, rest) = rest.split_once(':')?;
    let (seconds, milliseconds) = rest.split_once(',')?;
    let hours = hours.parse::<i64>().ok()?;
    let minutes = minutes.parse::<i64>().ok()?;
    let seconds = seconds.parse::<i64>().ok()?;
    let milliseconds = milliseconds.parse::<i64>().ok()?;
    if hours < 0
        || !(0..60).contains(&minutes)
        || !(0..60).contains(&seconds)
        || !(0..1000).contains(&milliseconds)
    {
        return None;
    }
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes * 60_000 + seconds * 1_000 + milliseconds)
}
