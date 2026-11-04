use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use image::{imageops::FilterType, DynamicImage, ImageReader};

use crate::core::cache::CacheManager;
use crate::core::error::{AppError, AppResult};
use crate::core::ffprobe::run_command_with_timeout;
use crate::core::media_tools::MediaTool;
use crate::models::{Asset, MediaType};

pub struct ThumbnailService {
    cache: CacheManager,
}

impl ThumbnailService {
    pub fn new(cache: CacheManager) -> Self {
        Self { cache }
    }

    pub fn remove_derivatives_for_asset(&self, asset_id: &str) -> AppResult<u64> {
        self.cache.remove_asset_derivatives(asset_id)
    }

    pub fn generate_for_asset(&self, asset: &Asset) -> AppResult<Option<PathBuf>> {
        let output_path = self.cache.thumbnail_path(&asset.id, "cover");
        if output_path.exists() {
            return Ok(Some(output_path));
        }

        match asset.media_type {
            MediaType::Image => generate_image_thumbnail(&asset.file_path, &output_path),
            MediaType::Video => {
                generate_video_thumbnail(&asset.file_path, &output_path, asset.duration_ms)
            }
            MediaType::Audio => Ok(None),
        }
    }
}

fn generate_image_thumbnail(input: &str, output: &Path) -> AppResult<Option<PathBuf>> {
    // Read through Rust's filesystem API before handing the bytes to `image`.
    // On Windows this keeps thumbnail generation working for library paths that
    // exceed the legacy MAX_PATH boundary, while `ImageReader::open` may reject
    // the same otherwise-readable path in some backends.
    let bytes = std::fs::read(input)?;
    let img = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| AppError::Other(format!("图片格式识别失败: {e}")))?;
    let img = img
        .decode()
        .map_err(|e| AppError::Other(format!("图片解码失败: {}", e)))?;
    let thumb = resize_to_cover(&img, 320, 180);
    save_jpeg(&thumb, output)?;
    Ok(Some(output.to_path_buf()))
}

fn generate_video_thumbnail(
    input: &str,
    output: &Path,
    duration_ms: Option<i64>,
) -> AppResult<Option<PathBuf>> {
    let Some(ffmpeg) = MediaTool::Ffmpeg.find() else {
        return Ok(None);
    };

    let time = duration_ms.map(|d| d as f64 / 2000.0).unwrap_or(1.0);
    let time_str = format!("{:.3}", time.max(0.1));

    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-nostdin",
            "-ss",
            &time_str,
            "-i",
            input,
            "-vf",
            "scale=320:180:force_original_aspect_ratio=decrease,pad=320:180:(ow-iw)/2:(oh-ih)/2",
            "-frames:v",
            "1",
            "-q:v",
            "2",
            "-y",
        ])
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let result =
        run_command_with_timeout(&mut command, Duration::from_secs(30), "视频封面生成超时");
    if result.is_ok_and(|result| result.status.success()) && output.is_file() {
        return Ok(Some(output.to_path_buf()));
    }
    if output.is_file() {
        std::fs::remove_file(output)?;
    }
    Ok(None)
}

fn resize_to_cover(img: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    let mut resized = img.resize(width, height, FilterType::Lanczos3);
    let (rw, rh) = (resized.width(), resized.height());
    if rw == width && rh == height {
        return resized;
    }
    // 如果比例不完全匹配，再做中心裁剪
    let cropped = resized.crop(
        (rw.saturating_sub(width)) / 2,
        (rh.saturating_sub(height)) / 2,
        width.min(rw),
        height.min(rh),
    );
    DynamicImage::from(cropped)
}

fn save_jpeg(img: &DynamicImage, output: &Path) -> AppResult<()> {
    let rgb = img.to_rgb8();
    rgb.save_with_format(output, image::ImageFormat::Jpeg)
        .map_err(|e| AppError::Other(format!("保存缩略图失败: {}", e)))?;
    Ok(())
}
