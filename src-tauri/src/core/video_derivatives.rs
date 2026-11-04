use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;

use crate::core::cache::CacheManager;
use crate::core::error::{AppError, AppResult};
use crate::core::ffprobe::run_command_with_timeout;
use crate::core::media_tools::MediaTool;
use crate::models::{IndexProfile, Segment};

const MAX_PREVIEW_DURATION_MS: i64 = 6_000;
const MAX_DERIVATIVE_SEGMENTS: usize = 120;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(90);

/// Generates a representative JPEG and a small playable MP4 per detected segment.
///
/// A missing FFmpeg is an expected optional-dependency case: the caller still gets
/// persisted shot boundaries, just without preview media. Other FFmpeg failures are
/// returned so the UI can explain that the original media is still intact.
pub fn generate_for_segments(
    cache: &CacheManager,
    source: &Path,
    asset_id: &str,
    segments: &mut [Segment],
    profile: &IndexProfile,
) -> AppResult<bool> {
    let Some(ffmpeg) = MediaTool::Ffmpeg.find() else {
        return Ok(false);
    };
    for segment in segments.iter_mut().take(MAX_DERIVATIVE_SEGMENTS) {
        let keyframe = cache.keyframe_path(asset_id, segment.segment_index);
        let preview = cache.segment_preview_path(asset_id, segment.segment_index);
        let timestamp_ms = representative_timestamp_ms(segment.start_ms, segment.duration_ms);

        run_ffmpeg(&ffmpeg, source, &keyframe_args(timestamp_ms, &keyframe))?;
        run_ffmpeg(
            &ffmpeg,
            source,
            &preview_args(segment.start_ms, segment.duration_ms, &preview),
        )?;

        let mut metrics = quality_metrics(&keyframe)?;
        if *profile == IndexProfile::Precise && segment.duration_ms >= 1_200 {
            let extra_frame = keyframe.with_file_name(format!("sample-{}.jpg", segment.id));
            let mut samples = 1.0;
            for offset in [segment.duration_ms / 4, segment.duration_ms * 3 / 4] {
                let timestamp = segment.start_ms + offset;
                if timestamp.abs_diff(timestamp_ms) < 150 {
                    continue;
                }
                let result = run_ffmpeg(&ffmpeg, source, &keyframe_args(timestamp, &extra_frame))
                    .and_then(|()| quality_metrics(&extra_frame));
                let _ = std::fs::remove_file(&extra_frame);
                match result {
                    Ok(sample) => {
                        samples += 1.0;
                        metrics.0 += sample.0;
                        metrics.1 += sample.1;
                        metrics.2 += sample.2;
                        metrics.3 |= sample.3;
                        metrics.4 |= sample.4;
                    }
                    Err(error) => log::warn!("额外代表帧分析失败: {error}"),
                }
            }
            metrics.0 /= samples;
            metrics.1 /= samples;
            metrics.2 /= samples;
        }
        let (black_frame_score, blur_score, quality_score, subtitle_present, game_ui) = metrics;
        let now = Utc::now().timestamp_millis();
        segment.representative_frame_path = Some(keyframe.to_string_lossy().to_string());
        segment.thumbnail_path = Some(keyframe.to_string_lossy().to_string());
        segment.preview_path = Some(preview.to_string_lossy().to_string());
        segment.black_frame_score = Some(black_frame_score);
        segment.blur_score = Some(blur_score);
        segment.quality_score = Some(quality_score);
        segment.subtitle_present = Some(subtitle_present);
        segment.game_ui = Some(game_ui);
        segment.updated_at = now;
    }
    Ok(true)
}

fn run_ffmpeg(executable: &Path, source: &Path, args: &[String]) -> AppResult<()> {
    if let Some(parent) = args.last().and_then(|output| Path::new(output).parent()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut command = Command::new(executable);
    command
        .arg("-hide_banner")
        .arg("-y")
        .arg("-i")
        .arg(source)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = run_command_with_timeout(
        &mut command,
        PROCESS_TIMEOUT,
        "视频派生文件生成超时（90 秒）；已终止 FFmpeg 子进程",
    )?;
    if output.status.success() {
        Ok(())
    } else {
        let details = String::from_utf8_lossy(&output.stderr);
        let message = details.lines().last().unwrap_or("未知 FFmpeg 错误");
        Err(AppError::Other(format!(
            "FFmpeg 生成视频预览失败: {message}"
        )))
    }
}

fn representative_timestamp_ms(start_ms: i64, duration_ms: i64) -> i64 {
    start_ms.saturating_add((duration_ms.max(1) / 2).min(1_000))
}

fn preview_duration_ms(duration_ms: i64) -> i64 {
    duration_ms.clamp(250, MAX_PREVIEW_DURATION_MS)
}

fn keyframe_args(timestamp_ms: i64, output: &Path) -> Vec<String> {
    vec![
        "-ss".into(),
        format_seconds(timestamp_ms),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        "scale='min(640,iw)':-2".into(),
        "-q:v".into(),
        "3".into(),
        output.to_string_lossy().to_string(),
    ]
}

fn preview_args(start_ms: i64, duration_ms: i64, output: &Path) -> Vec<String> {
    vec![
        "-ss".into(),
        format_seconds(start_ms),
        "-t".into(),
        format_seconds(preview_duration_ms(duration_ms)),
        "-an".into(),
        "-vf".into(),
        "scale='min(640,iw)':-2,fps=15".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "32".into(),
        "-movflags".into(),
        "+faststart".into(),
        output.to_string_lossy().to_string(),
    ]
}

fn format_seconds(milliseconds: i64) -> String {
    format!("{:.3}", milliseconds.max(0) as f64 / 1000.0)
}

fn quality_metrics(path: &Path) -> AppResult<(f64, f64, f64, bool, bool)> {
    let image = image::open(path)?.to_luma8();
    let pixels = image.as_raw();
    if pixels.is_empty() {
        return Ok((1.0, 1.0, 0.0, false, false));
    }
    let black = pixels.iter().filter(|&&pixel| pixel <= 16).count() as f64 / pixels.len() as f64;
    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut edges = 0.0;
    let mut comparisons = 0usize;
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if x + 1 < width {
                edges += (pixels[index] as f64 - pixels[index + 1] as f64).abs();
                comparisons += 1;
            }
            if y + 1 < height {
                edges += (pixels[index] as f64 - pixels[index + width] as f64).abs();
                comparisons += 1;
            }
        }
    }
    let detail = (edges / comparisons.max(1) as f64 / 28.0).clamp(0.0, 1.0);
    let blur = 1.0 - detail;
    let quality = ((1.0 - black) * 0.6 + detail * 0.4).clamp(0.0, 1.0);
    Ok((
        black,
        blur,
        quality,
        likely_subtitle(&image),
        likely_game_ui(&image),
    ))
}

/// A deliberately conservative local subtitle heuristic. It only looks for a
/// small, horizontally distributed cluster of bright pixels in the lower third
/// of a representative frame; it is not OCR and can be disabled by omitting
/// FFmpeg-derived keyframes.
fn likely_subtitle(image: &image::GrayImage) -> bool {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 32 || height < 24 {
        return false;
    }
    let start_y = height * 2 / 3;
    let region_height = height - start_y;
    let pixels = image.as_raw();
    let mut bright = 0usize;
    let mut dark = 0usize;
    let mut occupied_columns = vec![false; width];
    for y in start_y..height {
        for x in 0..width {
            let value = pixels[y * width + x];
            if value >= 220 {
                bright += 1;
                occupied_columns[x] = true;
            }
            if value <= 50 {
                dark += 1;
            }
        }
    }
    let area = width * region_height;
    let bright_ratio = bright as f64 / area as f64;
    let dark_ratio = dark as f64 / area as f64;
    let horizontal_coverage =
        occupied_columns.into_iter().filter(|value| *value).count() as f64 / width as f64;
    (0.003..=0.16).contains(&bright_ratio) && dark_ratio >= 0.12 && horizontal_coverage >= 0.18
}

/// A deliberately narrow local HUD hint. It requires separate, compact
/// high-contrast clusters in *both* lower corners of the representative frame.
/// This avoids treating central subtitles or a single bright scene object as UI;
/// it is not a general game-interface or menu classifier.
fn likely_game_ui(image: &image::GrayImage) -> bool {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 48 || height < 32 {
        return false;
    }
    let corner_width = (width * 3 / 10).max(12);
    let start_y = height * 3 / 5;
    hud_corner_active(image, 0, corner_width, start_y, height)
        && hud_corner_active(image, width - corner_width, width, start_y, height)
}

fn hud_corner_active(
    image: &image::GrayImage,
    start_x: usize,
    end_x: usize,
    start_y: usize,
    end_y: usize,
) -> bool {
    let width = image.width() as usize;
    let pixels = image.as_raw();
    let mut bright = 0usize;
    let mut dark = 0usize;
    let mut occupied_columns = vec![false; end_x - start_x];
    let mut occupied_rows = vec![false; end_y - start_y];
    for y in start_y..end_y {
        for x in start_x..end_x {
            let value = pixels[y * width + x];
            if value >= 220 {
                bright += 1;
                occupied_columns[x - start_x] = true;
                occupied_rows[y - start_y] = true;
            }
            if value <= 50 {
                dark += 1;
            }
        }
    }
    let area = (end_x - start_x) * (end_y - start_y);
    let bright_ratio = bright as f64 / area as f64;
    let dark_ratio = dark as f64 / area as f64;
    let column_coverage = occupied_columns.into_iter().filter(|value| *value).count() as f64
        / (end_x - start_x) as f64;
    let row_coverage =
        occupied_rows.into_iter().filter(|value| *value).count() as f64 / (end_y - start_y) as f64;
    (0.01..=0.28).contains(&bright_ratio)
        && dark_ratio >= 0.12
        && (0.08..=0.75).contains(&column_coverage)
        && (0.08..=0.75).contains(&row_coverage)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        keyframe_args, likely_game_ui, likely_subtitle, preview_args, preview_duration_ms,
        representative_timestamp_ms,
    };

    #[test]
    fn caps_preview_and_keeps_short_shots_playable() {
        assert_eq!(preview_duration_ms(10_000), 6_000);
        assert_eq!(preview_duration_ms(100), 250);
    }

    #[test]
    fn chooses_a_frame_inside_the_segment() {
        assert_eq!(representative_timestamp_ms(4_000, 8_000), 5_000);
        assert_eq!(representative_timestamp_ms(4_000, 300), 4_150);
    }

    #[test]
    fn detects_only_two_corner_hud_shapes() {
        let mut image = image::GrayImage::new(64, 48);
        for y in 32..38 {
            for x in 2..10 {
                image.put_pixel(x, y, image::Luma([255]));
            }
            for x in 54..62 {
                image.put_pixel(x, y, image::Luma([255]));
            }
        }
        assert!(likely_game_ui(&image));

        let mut single_corner = image::GrayImage::new(64, 48);
        for y in 32..38 {
            for x in 2..10 {
                single_corner.put_pixel(x, y, image::Luma([255]));
            }
        }
        assert!(!likely_game_ui(&single_corner));
    }

    #[test]
    fn ffmpeg_arguments_never_require_a_shell() {
        let keyframe = keyframe_args(1_250, Path::new("C:/cache/中文 frame.jpg"));
        let preview = preview_args(2_000, 10_000, Path::new("C:/cache/中文 preview.mp4"));
        assert_eq!(keyframe[0..2], ["-ss", "1.250"]);
        assert!(preview.windows(2).any(|pair| pair == ["-t", "6.000"]));
        assert_eq!(
            preview.last().map(String::as_str),
            Some("C:/cache/中文 preview.mp4")
        );
    }

    #[test]
    fn subtitle_heuristic_requires_a_small_lower_high_contrast_cluster() {
        let mut subtitle_frame = image::GrayImage::from_pixel(120, 80, image::Luma([0]));
        for y in 62..66 {
            for x in 20..100 {
                subtitle_frame.put_pixel(x, y, image::Luma([255]));
            }
        }
        assert!(likely_subtitle(&subtitle_frame));
        assert!(!likely_subtitle(&image::GrayImage::from_pixel(
            120,
            80,
            image::Luma([0])
        )));
    }
}
