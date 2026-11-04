use std::path::Path;

use image::{imageops, Rgb, RgbImage};

use crate::core::cache::CacheManager;
use crate::core::error::{AppError, AppResult};
use crate::models::{Asset, SelectItem};

pub fn write_csv(path: &Path, assets: &[Asset]) -> AppResult<()> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("csv"))
        != Some(true)
    {
        return Err(AppError::Other("导出文件必须使用 .csv 扩展名".to_string()));
    }
    let mut output =
        String::from("file_name,file_path,media_type,duration_ms,start_timecode,end_timecode\r\n");
    for asset in assets {
        let duration = asset.duration_ms.unwrap_or(0);
        output.push_str(&format!(
            "{},{},{},{},{},{}\r\n",
            csv_escape(&asset.file_name),
            csv_escape(&asset.file_path),
            asset.media_type.as_str(),
            duration,
            timecode(0),
            timecode(duration),
        ));
    }
    std::fs::write(path, output)?;
    Ok(())
}

pub fn write_json(path: &Path, assets: &[Asset]) -> AppResult<()> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("json"))
        != Some(true)
    {
        return Err(AppError::Other("导出文件必须使用 .json 扩展名".to_string()));
    }
    let items: Vec<serde_json::Value> = assets.iter().map(|asset| serde_json::json!({
        "file_name": asset.file_name, "file_path": asset.file_path, "media_type": asset.media_type.as_str(),
        "duration_ms": asset.duration_ms, "start_timecode": timecode(0), "end_timecode": timecode(asset.duration_ms.unwrap_or(0)),
    })).collect();
    std::fs::write(path, serde_json::to_string_pretty(&items)?)?;
    Ok(())
}

pub fn write_select_items_csv(path: &Path, items: &[SelectItem]) -> AppResult<()> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("csv"))
        != Some(true)
    {
        return Err(AppError::Other("导出文件必须使用 .csv 扩展名".to_string()));
    }
    let mut output = String::from(
        "file_name,file_path,media_type,start_timecode,end_timecode,rating,note,tags\r\n",
    );
    for item in items {
        let (start, end) = select_range(item);
        output.push_str(&format!(
            "{},{},{},{},{},{},{},{}\r\n",
            csv_escape(&item.asset.file_name),
            csv_escape(&item.asset.file_path),
            item.asset.media_type.as_str(),
            timecode(start),
            timecode(end),
            item.rating
                .map(|value| value.to_string())
                .unwrap_or_default(),
            csv_escape(item.note.as_deref().unwrap_or("")),
            csv_escape(&item.tags.join(" | "))
        ));
    }
    std::fs::write(path, output)?;
    Ok(())
}

pub fn write_select_contact_sheet_png(
    path: &Path,
    items: &[SelectItem],
    cache: &CacheManager,
) -> AppResult<()> {
    require_extension(path, "png")?;
    const TILE_WIDTH: u32 = 240;
    const TILE_HEIGHT: u32 = 150;
    const GAP: u32 = 8;
    const COLUMNS: u32 = 4;
    let rows = (items.len().max(1) as u32).div_ceil(COLUMNS);
    let width = COLUMNS * TILE_WIDTH + (COLUMNS + 1) * GAP;
    let height = rows * TILE_HEIGHT + (rows + 1) * GAP;
    let mut output = RgbImage::from_pixel(width, height, Rgb([24, 24, 27]));
    for (index, item) in items.iter().enumerate() {
        let source = item
            .segment
            .as_ref()
            .and_then(|segment| segment.thumbnail_path.as_ref())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| cache.thumbnail_path(&item.asset.id, "cover"));
        let Ok(source_image) = image::open(source) else {
            continue;
        };
        let thumbnail = source_image.thumbnail(TILE_WIDTH, TILE_HEIGHT).to_rgb8();
        let column = index as u32 % COLUMNS;
        let row = index as u32 / COLUMNS;
        let x = GAP + column * (TILE_WIDTH + GAP) + (TILE_WIDTH - thumbnail.width()) / 2;
        let y = GAP + row * (TILE_HEIGHT + GAP) + (TILE_HEIGHT - thumbnail.height()) / 2;
        imageops::replace(&mut output, &thumbnail, x.into(), y.into());
    }
    output.save(path)?;
    Ok(())
}

/// Writes an offline, print-friendly review sheet. Thumbnails are embedded so
/// the sheet remains usable when it is sent outside the local cache directory.
pub fn write_select_contact_sheet_html(
    path: &Path,
    items: &[SelectItem],
    cache: &CacheManager,
) -> AppResult<()> {
    require_extension(path, "html")?;
    let mut output = String::from(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>SceneWeaver Selects Contact Sheet</title><style>body{margin:32px;background:#18181b;color:#f4f4f5;font:14px/1.45 system-ui,-apple-system,Segoe UI,sans-serif}h1{margin:0 0 4px;font-size:24px}.summary{margin:0 0 24px;color:#a1a1aa}.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:16px}.card{overflow:hidden;border:1px solid #3f3f46;border-radius:10px;background:#27272a}.thumb{width:100%;aspect-ratio:16/9;display:grid;place-items:center;background:#18181b;object-fit:contain}.missing{color:#a1a1aa}.body{padding:12px}.name{margin:0 0 8px;font-weight:700;overflow-wrap:anywhere}.meta{margin:4px 0;color:#d4d4d8;overflow-wrap:anywhere}.label{color:#a1a1aa}@media print{body{margin:12mm;background:#fff;color:#18181b}.card{border-color:#a1a1aa;background:#fff}.thumb{background:#f4f4f5}.summary,.label{color:#52525b}.meta{color:#27272a}}</style></head><body><h1>SceneWeaver 选片联系表</h1>",
    );
    output.push_str(&format!(
        "<p class=\"summary\">共 {} 条 · 导出时间范围遵循推荐入/出点，随后使用片段范围。</p><main class=\"grid\">",
        items.len()
    ));
    for (index, item) in items.iter().enumerate() {
        let (start, end) = select_range(item);
        let thumbnail_path = item
            .segment
            .as_ref()
            .and_then(|segment| segment.thumbnail_path.as_ref())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| cache.thumbnail_path(&item.asset.id, "cover"));
        output.push_str("<article class=\"card\">");
        if let Some(data_uri) = thumbnail_data_uri(&thumbnail_path) {
            output.push_str(&format!(
                "<img class=\"thumb\" src=\"{}\" alt=\"{}\">",
                data_uri,
                html_escape(&item.asset.file_name)
            ));
        } else {
            output.push_str("<div class=\"thumb missing\">缩略图不可用</div>");
        }
        output.push_str("<div class=\"body\">");
        output.push_str(&format!(
            "<p class=\"name\">#{:02} {}</p><p class=\"meta\"><span class=\"label\">范围：</span>{} — {}</p><p class=\"meta\"><span class=\"label\">类型：</span>{}</p><p class=\"meta\"><span class=\"label\">路径：</span>{}</p>",
            index + 1,
            html_escape(&item.asset.file_name),
            timecode(start),
            timecode(end),
            html_escape(item.asset.media_type.as_str()),
            html_escape(&item.asset.file_path),
        ));
        if let Some(rating) = item.rating {
            output.push_str(&format!(
                "<p class=\"meta\"><span class=\"label\">评分：</span>{}/5</p>",
                rating
            ));
        }
        if let Some(note) = item.note.as_deref().filter(|note| !note.trim().is_empty()) {
            output.push_str(&format!(
                "<p class=\"meta\"><span class=\"label\">备注：</span>{}</p>",
                html_escape(note)
            ));
        }
        if !item.tags.is_empty() {
            output.push_str(&format!(
                "<p class=\"meta\"><span class=\"label\">标签：</span>{}</p>",
                html_escape(&item.tags.join(" · "))
            ));
        }
        output.push_str("</div></article>");
    }
    output.push_str("</main></body></html>\n");
    std::fs::write(path, output)?;
    Ok(())
}

pub fn write_select_items_json(path: &Path, items: &[SelectItem]) -> AppResult<()> {
    require_extension(path, "json")?;
    let values: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            let (start, end) = select_range(item);
            serde_json::json!({
                "file_name": item.asset.file_name,
                "file_path": item.asset.file_path,
                "media_type": item.asset.media_type.as_str(),
                "start_ms": start,
                "end_ms": end,
                "start_timecode": timecode(start),
                "end_timecode": timecode(end),
                "rating": item.rating,
                "note": item.note,
                "tags": item.tags,
            })
        })
        .collect();
    std::fs::write(path, serde_json::to_string_pretty(&values)?)?;
    Ok(())
}

pub fn write_select_items_edl(path: &Path, items: &[SelectItem]) -> AppResult<()> {
    require_extension(path, "edl")?;
    let mut output = String::from("TITLE: SceneWeaver Selects\r\nFCM: NON-DROP FRAME\r\n\r\n");
    let mut timeline_frame = 0_i64;
    for (index, item) in items.iter().enumerate() {
        let (start, end) = select_range(item);
        let source_in = frames_to_edl_timecode(milliseconds_to_frames(start));
        let duration_frames = milliseconds_to_frames(end - start).max(1);
        let source_out = frames_to_edl_timecode(milliseconds_to_frames(start) + duration_frames);
        let record_in = frames_to_edl_timecode(timeline_frame);
        let record_out = frames_to_edl_timecode(timeline_frame + duration_frames);
        output.push_str(&format!(
            "{:03}  AX       V     C        {source_in} {source_out} {record_in} {record_out}\r\n* FROM CLIP NAME: {}\r\n\r\n",
            index + 1,
            item.asset.file_name
        ));
        timeline_frame += duration_frames;
    }
    std::fs::write(path, output)?;
    Ok(())
}

pub fn write_select_items_fcpxml(path: &Path, items: &[SelectItem]) -> AppResult<()> {
    require_extension(path, "fcpxml")?;
    let total_ms: i64 = items
        .iter()
        .map(|item| {
            let (start, end) = select_range(item);
            (end - start).max(1)
        })
        .sum();
    let timeline_fps = items
        .iter()
        .find_map(|item| valid_fps(item.asset.fps))
        .unwrap_or(30.0);
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.10\"><resources><format id=\"format-timeline\" frameDuration=\"{}\" width=\"1920\" height=\"1080\"/>\n",
        fcpx_frame_duration(timeline_fps)
    );
    for (index, item) in items.iter().enumerate() {
        let (_, end) = select_range(item);
        let source_duration = item.asset.duration_ms.unwrap_or(end).max(end).max(1);
        append_fcpx_asset(&mut output, index + 1, &item.asset, source_duration);
    }
    output.push_str(&format!("</resources><library><event name=\"SceneWeaver\"><project name=\"SceneWeaver Selects\"><sequence duration=\"{}\" format=\"format-timeline\"><spine>\n", fcpx_duration(total_ms)));
    let mut offset = 0_i64;
    for (index, item) in items.iter().enumerate() {
        let (start, end) = select_range(item);
        let duration = (end - start).max(1);
        output.push_str(&format!(
            "<asset-clip ref=\"asset-{}\" name=\"{}\" offset=\"{}\" start=\"{}\" duration=\"{}\"/>\n",
            index + 1,
            xml_escape(&item.asset.file_name),
            fcpx_duration(offset),
            fcpx_duration(start),
            fcpx_duration(duration)
        ));
        offset += duration;
    }
    output.push_str("</spine></sequence></project></event></library></fcpxml>\n");
    std::fs::write(path, output)?;
    Ok(())
}

fn select_range(item: &SelectItem) -> (i64, i64) {
    let start = item
        .recommended_in_ms
        .or_else(|| item.segment.as_ref().map(|segment| segment.start_ms))
        .unwrap_or(0);
    let end = item
        .recommended_out_ms
        .or_else(|| item.segment.as_ref().map(|segment| segment.end_ms))
        .unwrap_or_else(|| item.asset.duration_ms.unwrap_or(0));
    (start, end.max(start))
}

fn require_extension(path: &Path, extension: &str) -> AppResult<()> {
    if path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(extension))
        != Some(true)
    {
        return Err(AppError::Other(format!(
            "导出文件必须使用 .{extension} 扩展名"
        )));
    }
    Ok(())
}

fn thumbnail_data_uri(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mime = match path.extension().and_then(|extension| extension.to_str())? {
        extension if extension.eq_ignore_ascii_case("png") => "image/png",
        extension if extension.eq_ignore_ascii_case("webp") => "image/webp",
        _ => "image/jpeg",
    };
    Some(format!("data:{mime};base64,{}", base64_encode(&bytes)))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 3) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((second & 15) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(third & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn write_edl(path: &Path, assets: &[Asset]) -> AppResult<()> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("edl"))
        != Some(true)
    {
        return Err(AppError::Other("导出文件必须使用 .edl 扩展名".to_string()));
    }
    let mut output = String::from("TITLE: SceneWeaver Selects\r\nFCM: NON-DROP FRAME\r\n\r\n");
    let mut timeline_frame = 0_i64;
    for (index, asset) in assets.iter().enumerate() {
        let duration_frames = milliseconds_to_frames(asset.duration_ms.unwrap_or(0)).max(1);
        let source_out = frames_to_edl_timecode(duration_frames);
        let record_in = frames_to_edl_timecode(timeline_frame);
        let record_out = frames_to_edl_timecode(timeline_frame + duration_frames);
        output.push_str(&format!("{:03}  AX       V     C        00:00:00:00 {source_out} {record_in} {record_out}\r\n* FROM CLIP NAME: {}\r\n\r\n", index + 1, asset.file_name));
        timeline_frame += duration_frames;
    }
    std::fs::write(path, output)?;
    Ok(())
}

pub fn write_fcpxml(path: &Path, assets: &[Asset]) -> AppResult<()> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("fcpxml"))
        != Some(true)
    {
        return Err(AppError::Other(
            "导出文件必须使用 .fcpxml 扩展名".to_string(),
        ));
    }
    let total_ms: i64 = assets
        .iter()
        .map(|asset| asset.duration_ms.unwrap_or(0).max(0))
        .sum();
    let timeline_fps = assets
        .iter()
        .find_map(|asset| valid_fps(asset.fps))
        .unwrap_or(30.0);
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.10\"><resources><format id=\"format-timeline\" frameDuration=\"{}\" width=\"1920\" height=\"1080\"/>\n",
        fcpx_frame_duration(timeline_fps)
    );
    for (index, asset) in assets.iter().enumerate() {
        append_fcpx_asset(
            &mut output,
            index + 1,
            asset,
            asset.duration_ms.unwrap_or(0),
        );
    }
    output.push_str(&format!("</resources><library><event name=\"SceneWeaver\"><project name=\"SceneWeaver Selects\"><sequence duration=\"{}\" format=\"format-timeline\"><spine>\n", fcpx_duration(total_ms)));
    let mut offset = 0_i64;
    for (index, asset) in assets.iter().enumerate() {
        let duration = asset.duration_ms.unwrap_or(0).max(0);
        output.push_str(&format!(
            "<asset-clip ref=\"asset-{}\" name=\"{}\" offset=\"{}\" start=\"0s\" duration=\"{}\"/>\n",
            index + 1,
            xml_escape(&asset.file_name),
            fcpx_duration(offset),
            fcpx_duration(duration)
        ));
        offset += duration;
    }
    output.push_str("</spine></sequence></project></event></library></fcpxml>\n");
    std::fs::write(path, output)?;
    Ok(())
}

fn fcpx_duration(milliseconds: i64) -> String {
    format!("{}/1000s", milliseconds.max(0))
}

fn append_fcpx_asset(output: &mut String, index: usize, asset: &Asset, duration_ms: i64) {
    let format_id = format!("format-asset-{index}");
    let format_attribute = if asset.media_type.as_str() == "audio" {
        String::new()
    } else {
        output.push_str(&fcpx_format_resource(&format_id, asset));
        format!(" format=\"{format_id}\"")
    };
    output.push_str(&format!(
        "<asset id=\"asset-{index}\" name=\"{}\" start=\"0s\" duration=\"{}\"{} {}><media-rep kind=\"original-media\" src=\"{}\"/></asset>\n",
        xml_escape(&asset.file_name),
        fcpx_duration(duration_ms),
        format_attribute,
        fcpx_media_attributes(asset),
        file_url(&asset.file_path),
    ));
}

fn valid_fps(fps: Option<f64>) -> Option<f64> {
    fps.filter(|value| value.is_finite() && *value >= 1.0 && *value <= 240.0)
}

fn fcpx_frame_duration(fps: f64) -> String {
    if (fps - 23.976).abs() < 0.02 {
        "1001/24000s".to_string()
    } else if (fps - 29.97).abs() < 0.02 {
        "1001/30000s".to_string()
    } else if (fps - 59.94).abs() < 0.02 {
        "1001/60000s".to_string()
    } else {
        format!("1/{}s", fps.round().clamp(1.0, 240.0) as i64)
    }
}

fn fcpx_format_resource(id: &str, asset: &Asset) -> String {
    let width = asset.width.unwrap_or(1920).max(1);
    let height = asset.height.unwrap_or(1080).max(1);
    let fps = valid_fps(asset.fps).unwrap_or(30.0);
    format!(
        "<format id=\"{}\" frameDuration=\"{}\" width=\"{}\" height=\"{}\"/>\n",
        id,
        fcpx_frame_duration(fps),
        width,
        height
    )
}
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn file_url(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let (prefix, path) = if let Some(unc) = normalized.strip_prefix("//?/UNC/").or_else(|| {
        normalized
            .strip_prefix("//")
            .filter(|path| !path.starts_with("?/"))
    }) {
        ("file://", unc)
    } else if let Some(path) = normalized.strip_prefix("//?/") {
        ("file:///", path)
    } else if normalized.starts_with('/') {
        ("file://", normalized.as_str())
    } else {
        ("file:///", normalized.as_str())
    };
    let mut encoded = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{prefix}{encoded}")
}

fn fcpx_media_attributes(asset: &Asset) -> &'static str {
    match asset.media_type.as_str() {
        "audio" => "hasAudio=\"1\"",
        "image" => "hasVideo=\"1\"",
        _ => "hasVideo=\"1\" hasAudio=\"1\"",
    }
}

fn milliseconds_to_frames(milliseconds: i64) -> i64 {
    ((milliseconds.max(0) as f64 / 1000.0) * 30.0).round() as i64
}
pub fn frames_to_edl_timecode(frames: i64) -> String {
    let frames = frames.max(0);
    let total_seconds = frames / 30;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        total_seconds / 3600,
        (total_seconds / 60) % 60,
        total_seconds % 60,
        frames % 30
    )
}

fn csv_escape(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub fn timecode(milliseconds: i64) -> String {
    let total_seconds = milliseconds.max(0) / 1000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total_seconds / 3600,
        (total_seconds / 60) % 60,
        total_seconds % 60,
        milliseconds.max(0) % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::{csv_escape, fcpx_frame_duration, file_url, timecode};

    #[test]
    fn formats_milliseconds_as_timecode() {
        assert_eq!(timecode(3_661_234), "01:01:01.234");
    }

    #[test]
    fn escapes_csv_quotes() {
        assert_eq!(csv_escape("a,\"b\""), "\"a,\"\"b\"\"\"");
    }

    #[test]
    fn formats_edl_timecode_at_thirty_fps() {
        assert_eq!(super::frames_to_edl_timecode(30 * 61 + 12), "00:01:01:12");
    }

    #[test]
    fn escapes_fcpxml_values() {
        assert_eq!(super::xml_escape("A&B"), "A&amp;B");
    }

    #[test]
    fn encodes_non_ascii_and_reserved_file_url_characters() {
        assert_eq!(
            file_url(r"C:\素材 库\雨夜#1%.mp4"),
            "file:///C:/%E7%B4%A0%E6%9D%90%20%E5%BA%93/%E9%9B%A8%E5%A4%9C%231%25.mp4"
        );
    }

    #[test]
    fn preserves_common_ntsc_frame_durations_for_fcpxml() {
        assert_eq!(fcpx_frame_duration(23.976), "1001/24000s");
        assert_eq!(fcpx_frame_duration(29.97), "1001/30000s");
        assert_eq!(fcpx_frame_duration(59.94), "1001/60000s");
        assert_eq!(fcpx_frame_duration(25.0), "1/25s");
    }
}
