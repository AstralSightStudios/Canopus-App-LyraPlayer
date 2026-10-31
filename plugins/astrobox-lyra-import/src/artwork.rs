//! Cover preparation: the watch shows a 176x176 rounded cover and fades the page
//! from one colour to another behind it, so the importer sends exactly that -- an LVGL
//! v9 ARGB8888 BIN with transparent rounded corners and the two colours picked from
//! the picture.

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

pub const COVER_SIZE: u32 = 176;
pub const COVER_CORNER_RADIUS: u32 = 26;
pub const COVER_BIN_BYTES: u64 = 12 + COVER_SIZE as u64 * COVER_SIZE as u64 * 4;

const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 2048;
const MAX_DECODE_ALLOC: u64 = 32 * 1024 * 1024;

/// The colours the player fades between, `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub top: u32,
    pub bottom: u32,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            top: 0x3A3F58,
            bottom: 0x14161F,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreparedArtwork {
    pub cover_path: String,
    pub cover_size: u64,
    pub palette: Palette,
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
    let palette = palette_of(&cover);
    let cover_path = output_directory.join("cover.bin");
    write_lvgl_v9(&cover_path, &cover)?;
    let cover_size = file_size(&cover_path)?;
    if cover_size != COVER_BIN_BYTES {
        return Err("封面 BIN 长度校验失败".to_string());
    }
    Ok(PreparedArtwork {
        cover_path: path_text(&cover_path)?,
        cover_size,
        palette,
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
    if image.width() == 0
        || image.height() == 0
        || image.width() > MAX_SOURCE_DIMENSION
        || image.height() > MAX_SOURCE_DIMENSION
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
    let mut cover = imageops::resize(&crop, COVER_SIZE, COVER_SIZE, FilterType::Lanczos3);
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
        let outside_distance = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
        let inside_distance = dx.max(dy).min(0.0);
        let signed_distance = outside_distance + inside_distance - radius;
        let t = (-signed_distance / AA_WIDTH).clamp(0.0, 1.0);
        let smooth = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        pixel[3] = (f32::from(pixel[3]) * smooth).round() as u8;
    }
}

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Hue (0..360), saturation and value (0..1) of an sRGB colour.
fn to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    (hue, if max == 0.0 { 0.0 } else { delta / max }, max)
}

fn from_hsv(hue: f32, saturation: f32, value: f32) -> u32 {
    let chroma = value * saturation;
    let sector = (hue / 60.0).rem_euclid(6.0);
    let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = value - chroma;
    let byte = |channel: f32| ((channel + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(r) << 16) | (byte(g) << 8) | byte(b)
}

/// Picks the page colours: the most vivid hue of the cover, darkened until white text
/// reads on it at the top and nearly black at the bottom. A grey cover gets grey tones.
pub fn palette_of(cover: &RgbaImage) -> Palette {
    let small = imageops::resize(cover, 24, 24, FilterType::Triangle);
    let mut bins = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); 12]; // weight, sum of r, g, b
    let mut average = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for pixel in small.pixels() {
        if pixel[3] < 128 {
            continue;
        }
        let (r, g, b) = (
            f32::from(pixel[0]) / 255.0,
            f32::from(pixel[1]) / 255.0,
            f32::from(pixel[2]) / 255.0,
        );
        let (hue, saturation, value) = to_hsv(r, g, b);
        average = (average.0 + 1.0, average.1 + r, average.2 + g, average.3 + b);
        let weight = saturation * saturation * value;
        let bin = &mut bins[((hue / 30.0) as usize).min(11)];
        *bin = (bin.0 + weight, bin.1 + r * weight, bin.2 + g * weight, bin.3 + b * weight);
    }
    if average.0 == 0.0 {
        return Palette::default();
    }
    let strongest = bins
        .iter()
        .copied()
        .fold((0.0f32, 0.0f32, 0.0f32, 0.0f32), |best, bin| if bin.0 > best.0 { bin } else { best });
    // Grey picture: no hue stands out; use the average tone.
    let (hue, saturation) = if strongest.0 < 0.35 {
        let (hue, saturation, _) =
            to_hsv(average.1 / average.0, average.2 / average.0, average.3 / average.0);
        (hue, saturation.min(0.25))
    } else {
        let (hue, saturation, _) =
            to_hsv(strongest.1 / strongest.0, strongest.2 / strongest.0, strongest.3 / strongest.0);
        (hue, saturation.clamp(0.35, 0.80))
    };
    Palette {
        top: from_hsv(hue, saturation * 0.92, 0.46),
        bottom: from_hsv(hue, (saturation * 0.85).min(0.7), 0.11),
    }
}

// ---------------------------------------------------------------------------
// LVGL BIN
// ---------------------------------------------------------------------------

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

    fn solid(color: [u8; 4]) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(240, 240, Rgba(color)))
    }

    #[test]
    fn encoder_matches_lvgl_v9_bgra_layout() {
        let directory = unique_output_directory("artwork-test");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("pixel.bin");
        write_lvgl_v9(&path, &RgbaImage::from_pixel(1, 1, Rgba([0x11, 0x22, 0x33, 0x44]))).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            [0x19, 0x10, 0, 0, 1, 0, 1, 0, 4, 0, 0, 0, 0x33, 0x22, 0x11, 0x44]
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn cover_is_square_with_transparent_corners_and_opaque_centre() {
        let wide = DynamicImage::ImageRgba8(RgbaImage::from_pixel(640, 320, Rgba([220, 80, 40, 255])));
        let cover = square_cover(&wide);
        assert_eq!(cover.dimensions(), (COVER_SIZE, COVER_SIZE));
        assert_eq!(cover.get_pixel(0, 0)[3], 0);
        assert_eq!(cover.get_pixel(COVER_SIZE - 1, 0)[3], 0);
        assert_eq!(cover.get_pixel(COVER_SIZE / 2, COVER_SIZE / 2)[3], 255);
        assert_eq!(cover.get_pixel(COVER_SIZE / 2, 2)[3], 255);
    }

    #[test]
    fn prepared_cover_has_the_exact_bin_size() {
        let directory = unique_output_directory("artwork-prepare-test");
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.png");
        RgbaImage::from_pixel(400, 300, Rgba([30, 120, 200, 255])).save(&source).unwrap();
        let prepared = prepare(&source, &directory.join("out")).unwrap();
        assert_eq!(prepared.cover_size, COVER_BIN_BYTES);
        let header = fs::read(&prepared.cover_path).unwrap();
        assert_eq!(&header[..4], &[0x19, 0x10, 0, 0]);
        assert_eq!(u16::from_le_bytes([header[4], header[5]]), COVER_SIZE as u16);
        let _ = fs::remove_dir_all(directory);
    }

    fn luminance(color: u32) -> f32 {
        let channel = |shift: u32| ((color >> shift) & 0xFF) as f32 / 255.0;
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    #[test]
    fn palette_follows_the_hue_and_keeps_white_text_readable() {
        for (rgba, name) in [
            ([230, 40, 40, 255], "red"),
            ([40, 200, 90, 255], "green"),
            ([250, 240, 90, 255], "yellow"),
            ([240, 240, 255, 255], "near white"),
        ] {
            let palette = palette_of(&square_cover(&solid(rgba)));
            assert!(luminance(palette.top) < 0.45, "{name}: top is too bright");
            assert!(luminance(palette.bottom) < luminance(palette.top), "{name}: not darker below");
            assert!(luminance(palette.bottom) < 0.12, "{name}: bottom is not dark");
        }
        let red = palette_of(&square_cover(&solid([230, 40, 40, 255])));
        assert!((red.top >> 16) & 0xFF > (red.top >> 8) & 0xFF, "a red cover gives a reddish top");
        let green = palette_of(&square_cover(&solid([40, 200, 90, 255])));
        assert!((green.top >> 8) & 0xFF > (green.top >> 16) & 0xFF);
    }

    #[test]
    fn grey_and_transparent_pictures_get_neutral_colours() {
        let grey = palette_of(&square_cover(&solid([128, 128, 128, 255])));
        let spread = |color: u32| {
            let channels = [(color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF];
            channels.iter().max().unwrap() - channels.iter().min().unwrap()
        };
        assert!(spread(grey.top) < 40);
        assert_eq!(palette_of(&RgbaImage::new(8, 8)), Palette::default());
    }
}
