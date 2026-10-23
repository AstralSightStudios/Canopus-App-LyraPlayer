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
pub const COVER_CORNER_RADIUS: u32 = 24;
pub const BACKGROUND_HEIGHT: u32 = 520;
/// The bottom rows fade out so the background blends into the list below.
pub const BACKGROUND_FADE_SPAN: u32 = 40;
pub const LVGL_V9_FORMAT: &str = "lvgl-v9-argb8888-bin";
pub const COVER_BIN_BYTES: u64 = lvgl_v9_bin_bytes(COVER_WIDTH, COVER_HEIGHT);

const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 2048;
const MAX_DECODE_ALLOC: u64 = 32 * 1024 * 1024;
const BLUR_HEIGHT: u32 = 120;

/// The wearable the artwork is prepared for; it decides the background width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceProfile {
    /// 336 px wide. Also used for devices we cannot identify, which is what
    /// every import produced before per-device artwork.
    #[default]
    Band10Pro,
    /// 212 px wide.
    Band11,
}

impl DeviceProfile {
    pub const ALL: [Self; 2] = [Self::Band10Pro, Self::Band11];

    /// Identifies the device from the name AstroBox reports. Whitespace and
    /// letter case are ignored, so "小米手环 11" matches "小米手环11".
    pub fn from_device_name(name: &str) -> Self {
        const BAND_11: [&str; 3] = ["小米手环11", "小米手環11", "Xiaomi Smart Band 11"];
        const BAND_10_PRO: [&str; 3] = [
            "小米手环10 Pro",
            "小米手環10 Pro",
            "Xiaomi Smart Band 10 Pro",
        ];
        let name = normalized_device_name(name);
        let matches = |keys: &[&str]| {
            keys.iter()
                .any(|key| name.contains(&normalized_device_name(key)))
        };
        if matches(&BAND_11) {
            Self::Band11
        } else if matches(&BAND_10_PRO) {
            Self::Band10Pro
        } else {
            Self::default()
        }
    }

    pub const fn background_width(self) -> u32 {
        match self {
            Self::Band10Pro => 336,
            Self::Band11 => 212,
        }
    }

    pub const fn background_bin_bytes(self) -> u64 {
        lvgl_v9_bin_bytes(self.background_width(), BACKGROUND_HEIGHT)
    }
}

fn normalized_device_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

pub const fn lvgl_v9_bin_bytes(width: u32, height: u32) -> u64 {
    12 + width as u64 * height as u64 * 4
}

#[derive(Clone, Debug)]
pub struct PreparedBackground {
    pub path: String,
    pub size: u64,
    pub width: u32,
}

#[derive(Clone, Debug)]
pub struct PreparedArtwork {
    pub cover_path: String,
    pub cover_size: u64,
    pub background: Option<PreparedBackground>,
}

pub fn prepare(
    source: &Path,
    output_directory: &Path,
    profile: DeviceProfile,
) -> Result<PreparedArtwork, String> {
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
    let background = match background(&image, profile.background_width())
        .and_then(|background| write_lvgl_v9(&background_path, &background))
    {
        Ok(()) => {
            let size = file_size(&background_path)?;
            if size == profile.background_bin_bytes() {
                Some(PreparedBackground {
                    path: path_text(&background_path)?,
                    size,
                    width: profile.background_width(),
                })
            } else {
                let _ = fs::remove_file(&background_path);
                tracing::warn!("background BIN length mismatch; importing cover only");
                None
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&background_path);
            tracing::warn!("background generation skipped: {error}");
            None
        }
    };

    Ok(PreparedArtwork {
        cover_path: path_text(&cover_path)?,
        cover_size,
        background,
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
    let mut cover = imageops::resize(&crop, COVER_WIDTH, COVER_HEIGHT, FilterType::Lanczos3);
    apply_rounded_corner_alpha(&mut cover);
    cover
}

fn apply_rounded_corner_alpha(image: &mut RgbaImage) {
    let width = image.width() as f32;
    let height = image.height() as f32;
    let radius = COVER_CORNER_RADIUS.min(image.width().min(image.height()) / 2) as f32;
    let half_width = width / 2.0;
    let half_height = height / 2.0;
    let corner_x = half_width - radius;
    let corner_y = half_height - radius;
    const AA_WIDTH: f32 = 2.0;

    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let px = x as f32 + 0.5;
        let py = y as f32 + 0.5;
        let dx = (px - half_width).abs() - corner_x;
        let dy = (py - half_height).abs() - corner_y;
        let outside_x = dx.max(0.0);
        let outside_y = dy.max(0.0);
        let outside_distance = (outside_x * outside_x + outside_y * outside_y).sqrt();
        let inside_distance = dx.max(dy).min(0.0);
        let signed_distance = outside_distance + inside_distance - radius;
        let t = (-signed_distance / AA_WIDTH).clamp(0.0, 1.0);
        let smooth = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        pixel[3] = (f32::from(pixel[3]) * smooth).round() as u8;
    }
}

fn background(image: &DynamicImage, width: u32) -> Result<RgbaImage, String> {
    let source = image.to_rgba8();
    let (crop_width, crop_height) =
        aspect_fill_crop(source.width(), source.height(), width, BACKGROUND_HEIGHT)?;
    let x = (source.width() - crop_width) / 2;
    let y = (source.height() - crop_height) / 2;
    let crop = imageops::crop_imm(&source, x, y, crop_width, crop_height).to_image();
    let small = imageops::resize(&crop, (width / 4).max(1), BLUR_HEIGHT, FilterType::Triangle);
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

    let mut output = imageops::resize(&blurred, width, BACKGROUND_HEIGHT, FilterType::Triangle);
    let fade_start = BACKGROUND_HEIGHT - BACKGROUND_FADE_SPAN;
    for (_, y, pixel) in output.enumerate_pixels_mut() {
        let edge = y.abs_diff(BACKGROUND_HEIGHT / 2);
        let vignette = edge.saturating_mul(20) / (BACKGROUND_HEIGHT / 2).max(1);
        let keep = 40u32.saturating_sub(vignette.min(20));
        for channel in 0..3 {
            pixel[channel] = ((u32::from(pixel[channel]) * keep) / 100) as u8;
        }
        pixel[3] = if y < fade_start {
            255
        } else {
            let fade_rows = BACKGROUND_FADE_SPAN.saturating_sub(1).max(1);
            let remaining = BACKGROUND_HEIGHT.saturating_sub(y + 1);
            (remaining.min(fade_rows) * 255 / fade_rows) as u8
        };
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
        assert_eq!(aspect_fill_crop(800, 400, 336, 520), Ok((258, 400)));
        assert_eq!(aspect_fill_crop(400, 800, 336, 520), Ok((400, 619)));
        assert_eq!(aspect_fill_crop(800, 400, 212, 520), Ok((163, 400)));
    }

    #[test]
    fn device_names_select_the_background_profile() {
        for name in [
            "小米手环11",
            "小米手环11 A1B2",
            "小米手环 11",
            "小米手环\u{3000}11",
            "小米手環11",
            "小米手環 11 NFC",
            "Xiaomi Smart Band 11",
            "Xiaomi Smart Band 11 NFC",
            "XIAOMI SMART BAND 11",
            "Xiaomi  Smart Band11",
        ] {
            assert_eq!(
                DeviceProfile::from_device_name(name),
                DeviceProfile::Band11,
                "{name}"
            );
        }
        for name in [
            "小米手环10 Pro",
            "小米手环 10 Pro",
            "小米手環10 Pro 5C3D",
            "Xiaomi Smart Band 10 Pro",
            "xiaomi smart band 10 pro",
            "",
            "Xiaomi Watch S4",
        ] {
            assert_eq!(
                DeviceProfile::from_device_name(name),
                DeviceProfile::Band10Pro,
                "{name}"
            );
        }
        assert_eq!(DeviceProfile::Band10Pro.background_bin_bytes(), 698_892);
        assert_eq!(DeviceProfile::Band11.background_bin_bytes(), 440_972);
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
        let fade_start = BACKGROUND_HEIGHT - BACKGROUND_FADE_SPAN;
        for profile in DeviceProfile::ALL {
            let background = background(&image, profile.background_width()).unwrap();
            assert_eq!(
                background.dimensions(),
                (profile.background_width(), BACKGROUND_HEIGHT)
            );
            assert!(
                background
                    .enumerate_pixels()
                    .filter(|(_, y, _)| *y < fade_start)
                    .all(|(_, _, pixel)| pixel[3] == 255)
            );
            assert_eq!(background.get_pixel(0, fade_start)[3], 255);
            assert_eq!(background.get_pixel(0, BACKGROUND_HEIGHT - 1)[3], 0);
        }
    }

    #[test]
    fn prepared_background_matches_the_device_profile() {
        let directory = unique_output_directory("artwork-profile-test");
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.png");
        RgbaImage::from_pixel(400, 300, Rgba([30, 120, 200, 255]))
            .save(&source)
            .unwrap();
        for profile in DeviceProfile::ALL {
            let output = directory.join(format!("{profile:?}"));
            let prepared = prepare(&source, &output, profile).unwrap();
            assert_eq!(prepared.cover_size, COVER_BIN_BYTES);
            let background = prepared.background.unwrap();
            assert_eq!(background.width, profile.background_width());
            assert_eq!(background.size, profile.background_bin_bytes());
            let header = fs::read(&background.path).unwrap();
            assert_eq!(
                u16::from_le_bytes([header[4], header[5]]),
                profile.background_width() as u16
            );
            assert_eq!(
                u16::from_le_bytes([header[6], header[7]]),
                BACKGROUND_HEIGHT as u16
            );
        }
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rounded_cover_has_transparent_corners_and_opaque_center() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            240,
            240,
            Rgba([220, 80, 40, 255]),
        ));
        let cover = square_cover(&image);
        assert_eq!(cover.get_pixel(0, 0)[3], 0);
        assert_eq!(cover.get_pixel(COVER_WIDTH - 1, 0)[3], 0);
        assert_eq!(cover.get_pixel(COVER_WIDTH / 2, COVER_HEIGHT / 2)[3], 255);
        assert_eq!(cover.get_pixel(COVER_WIDTH / 2, 2)[3], 255);
    }

    #[test]
    fn background_alpha_fades_monotonically_after_480() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            640,
            320,
            Rgba([220, 80, 40, 100]),
        ));
        let fade_start = BACKGROUND_HEIGHT - BACKGROUND_FADE_SPAN;
        assert_eq!(fade_start, 480);
        let background = background(&image, DeviceProfile::Band10Pro.background_width()).unwrap();
        let mut previous = 255;
        for y in fade_start..BACKGROUND_HEIGHT {
            let alpha = background.get_pixel(0, y)[3];
            assert!(alpha <= previous);
            previous = alpha;
        }
        assert_eq!(background.get_pixel(0, fade_start)[3], 255);
        assert_eq!(background.get_pixel(0, BACKGROUND_HEIGHT - 1)[3], 0);
    }
}
