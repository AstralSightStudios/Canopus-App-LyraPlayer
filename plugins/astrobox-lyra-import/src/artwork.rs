use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use image::{
    DynamicImage, ImageReader, Limits, RgbaImage,
    imageops::{self, FilterType},
};

pub const COVER_WIDTH: u32 = 180;
pub const COVER_HEIGHT: u32 = 180;
pub const BACKGROUND_WIDTH: u32 = 336;
pub const BACKGROUND_HEIGHT: u32 = 480;
pub const LVGL_V9_FORMAT: &str = "lvgl-v9-argb8888-bin";
pub const COVER_BIN_BYTES: u64 = 12 + COVER_WIDTH as u64 * COVER_HEIGHT as u64 * 4;
pub const BACKGROUND_BIN_BYTES: u64 = 12 + BACKGROUND_WIDTH as u64 * BACKGROUND_HEIGHT as u64 * 4;

const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 2048;
const MAX_DECODE_ALLOC: u64 = 32 * 1024 * 1024;
const BLUR_WIDTH: u32 = 84;
const BLUR_HEIGHT: u32 = 120;

#[derive(Clone, Debug)]
pub struct PreparedArtwork {
    pub cover_path: String,
    pub cover_size: u64,
    pub background_path: Option<String>,
    pub background_size: Option<u64>,
}

pub fn prepare(source: &Path, output_directory: &Path) -> Result<PreparedArtwork, String> {
    let metadata = fs::metadata(source).map_err(|error| format!("无法读取封面：{error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_SOURCE_BYTES {
        return Err("封面为空或超过 4 MiB".to_string());
    }

    let image = decode(source)?;
    fs::create_dir_all(output_directory)
        .map_err(|error| format!("无法创建封面处理目录：{error}"))?;

    let cover = square_cover(&image);
    let cover_path = output_directory.join("cover.bin");
    write_lvgl_v9(&cover_path, &cover)?;
    let cover_size = file_size(&cover_path)?;
    if cover_size != COVER_BIN_BYTES {
        return Err("封面 BIN 长度校验失败".to_string());
    }

    let background_path = output_directory.join("background.bin");
    let (background_path, background_size) = match background(&image)
        .and_then(|background| write_lvgl_v9(&background_path, &background))
    {
        Ok(()) => {
            let size = file_size(&background_path)?;
            if size == BACKGROUND_BIN_BYTES {
                (Some(path_text(&background_path)?), Some(size))
            } else {
                let _ = fs::remove_file(&background_path);
                tracing::warn!("background BIN length mismatch; importing cover only");
                (None, None)
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&background_path);
            tracing::warn!("background generation skipped: {error}");
            (None, None)
        }
    };

    Ok(PreparedArtwork {
        cover_path: path_text(&cover_path)?,
        cover_size,
        background_path,
        background_size,
    })
}

pub fn unique_output_directory(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    PathBuf::from("media").join(format!("{prefix}-{nonce}"))
}

fn decode(source: &Path) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::open(source)
        .map_err(|error| format!("无法打开封面：{error}"))?
        .with_guessed_format()
        .map_err(|error| format!("无法识别封面格式：{error}"))?;
    let format = reader
        .format()
        .ok_or_else(|| "封面必须是 JPEG 或 PNG".to_string())?;
    if !matches!(format, image::ImageFormat::Jpeg | image::ImageFormat::Png) {
        return Err("封面必须是 JPEG 或 PNG".to_string());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| format!("封面解码失败：{error}"))?;
    let pixels = u64::from(image.width())
        .checked_mul(u64::from(image.height()))
        .ok_or_else(|| "封面尺寸溢出".to_string())?;
    if image.width() == 0
        || image.height() == 0
        || image.width() > MAX_SOURCE_DIMENSION
        || image.height() > MAX_SOURCE_DIMENSION
        || pixels > u64::from(MAX_SOURCE_DIMENSION) * u64::from(MAX_SOURCE_DIMENSION)
    {
        return Err("封面分辨率超过 2048×2048".to_string());
    }
    Ok(image)
}

fn square_cover(image: &DynamicImage) -> RgbaImage {
    let source = image.to_rgba8();
    let side = source.width().min(source.height());
    let x = (source.width() - side) / 2;
    let y = (source.height() - side) / 2;
    let crop = imageops::crop_imm(&source, x, y, side, side).to_image();
    imageops::resize(&crop, COVER_WIDTH, COVER_HEIGHT, FilterType::Lanczos3)
}

fn background(image: &DynamicImage) -> Result<RgbaImage, String> {
    let source = image.to_rgba8();
    let (crop_width, crop_height) = aspect_fill_crop(
        source.width(),
        source.height(),
        BACKGROUND_WIDTH,
        BACKGROUND_HEIGHT,
    )?;
    let x = (source.width() - crop_width) / 2;
    let y = (source.height() - crop_height) / 2;
    let crop = imageops::crop_imm(&source, x, y, crop_width, crop_height).to_image();
    let small = imageops::resize(&crop, BLUR_WIDTH, BLUR_HEIGHT, FilterType::Triangle);
    let mut blurred = imageops::blur(&small, 8.0);
    let (top, bottom) = half_colors(&small);

    for (x, y, pixel) in blurred.enumerate_pixels_mut() {
        let _ = x;
        let mix = if BLUR_HEIGHT <= 1 {
            0
        } else {
            y.saturating_mul(255) / (BLUR_HEIGHT - 1)
        };
        let gradient = [
            lerp(top[0], bottom[0], mix),
            lerp(top[1], bottom[1], mix),
            lerp(top[2], bottom[2], mix),
        ];
        for channel in 0..3 {
            let combined = (u16::from(pixel[channel]) * 3 + u16::from(gradient[channel])) / 4;
            pixel[channel] = saturate(combined as u8);
        }
        pixel[3] = 255;
    }

    let mut output = imageops::resize(
        &blurred,
        BACKGROUND_WIDTH,
        BACKGROUND_HEIGHT,
        FilterType::Triangle,
    );
    for (_, y, pixel) in output.enumerate_pixels_mut() {
        let edge = y.abs_diff(BACKGROUND_HEIGHT / 2);
        let vignette = edge.saturating_mul(20) / (BACKGROUND_HEIGHT / 2).max(1);
        let keep = 55u32.saturating_sub(vignette.min(20));
        for channel in 0..3 {
            pixel[channel] = ((u32::from(pixel[channel]) * keep) / 100) as u8;
        }
        pixel[3] = 255;
    }
    Ok(output)
}

fn aspect_fill_crop(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<(u32, u32), String> {
    if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
        return Err("封面尺寸无效".to_string());
    }
    let source_ratio = u64::from(source_width) * u64::from(target_height);
    let target_ratio = u64::from(target_width) * u64::from(source_height);
    if source_ratio > target_ratio {
        let width =
            (u64::from(source_height) * u64::from(target_width) / u64::from(target_height)) as u32;
        Ok((width.max(1).min(source_width), source_height))
    } else {
        let height =
            (u64::from(source_width) * u64::from(target_height) / u64::from(target_width)) as u32;
        Ok((source_width, height.max(1).min(source_height)))
    }
}

fn half_colors(image: &RgbaImage) -> ([u8; 3], [u8; 3]) {
    let split = image.height() / 2;
    (
        average_color(image, 0, split.max(1)),
        average_color(image, split, image.height()),
    )
}

fn average_color(image: &RgbaImage, start_y: u32, end_y: u32) -> [u8; 3] {
    let mut sums = [0u64; 3];
    let mut count = 0u64;
    for y in start_y..end_y {
        for x in 0..image.width() {
            let pixel = image.get_pixel(x, y);
            for channel in 0..3 {
                sums[channel] += u64::from(pixel[channel]);
            }
            count += 1;
        }
    }
    if count == 0 {
        return [0; 3];
    }
    [
        (sums[0] / count) as u8,
        (sums[1] / count) as u8,
        (sums[2] / count) as u8,
    ]
}

fn lerp(start: u8, end: u8, amount: u32) -> u8 {
    ((u32::from(start) * (255 - amount) + u32::from(end) * amount) / 255) as u8
}

fn saturate(value: u8) -> u8 {
    let centered = i16::from(value) - 128;
    (128 + centered * 11 / 10).clamp(0, 255) as u8
}

fn write_lvgl_v9(path: &Path, image: &RgbaImage) -> Result<(), String> {
    let width = u16::try_from(image.width()).map_err(|_| "BIN 宽度溢出".to_string())?;
    let height = u16::try_from(image.height()).map_err(|_| "BIN 高度溢出".to_string())?;
    let stride = width
        .checked_mul(4)
        .ok_or_else(|| "BIN stride 溢出".to_string())?;
    let file = File::create(path).map_err(|error| format!("无法创建 BIN：{error}"))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(&[0x19, 0x10, 0, 0])
        .and_then(|()| writer.write_all(&width.to_le_bytes()))
        .and_then(|()| writer.write_all(&height.to_le_bytes()))
        .and_then(|()| writer.write_all(&stride.to_le_bytes()))
        .and_then(|()| writer.write_all(&[0, 0]))
        .map_err(|error| format!("无法写入 BIN 头：{error}"))?;
    for pixel in image.pixels() {
        writer
            .write_all(&[pixel[2], pixel[1], pixel[0], pixel[3]])
            .map_err(|error| format!("无法写入 BIN 像素：{error}"))?;
    }
    writer
        .flush()
        .map_err(|error| format!("无法完成 BIN：{error}"))
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("无法读取 BIN 长度：{error}"))
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| "图片输出路径不是 UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn aspect_fill_uses_center_crop() {
        assert_eq!(aspect_fill_crop(800, 400, 336, 480), Ok((280, 400)));
        assert_eq!(aspect_fill_crop(400, 800, 336, 480), Ok((400, 571)));
    }

    #[test]
    fn encoder_matches_lvgl_v9_bgra_layout() {
        let directory = unique_output_directory("artwork-test");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("pixel.bin");
        let image = RgbaImage::from_pixel(1, 1, Rgba([0x11, 0x22, 0x33, 0x44]));
        write_lvgl_v9(&path, &image).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            bytes,
            [
                0x19, 0x10, 0, 0, 1, 0, 1, 0, 4, 0, 0, 0, 0x33, 0x22, 0x11, 0x44
            ]
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn generated_dimensions_and_alpha_are_fixed() {
        let image =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(640, 320, Rgba([220, 80, 40, 100])));
        let cover = square_cover(&image);
        assert_eq!(cover.dimensions(), (COVER_WIDTH, COVER_HEIGHT));
        let background = background(&image).unwrap();
        assert_eq!(
            background.dimensions(),
            (BACKGROUND_WIDTH, BACKGROUND_HEIGHT)
        );
        assert!(background.pixels().all(|pixel| pixel[3] == 255));
    }
}
