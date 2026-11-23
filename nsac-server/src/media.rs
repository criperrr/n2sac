//! Profile image sanitizing: every avatar/banner upload is fully decoded and re-encoded.
//!
//! - The real format is sniffed from the bytes (the declared MIME is ignored); only PNG, JPEG,
//!   WebP and GIF (first frame) decode. SVG, HTML, executables, polyglots and anything else fail.
//! - Decoding runs under strict limits (dimensions + allocation), so decompression bombs fail fast.
//! - The output is a brand-new JPEG built from pixels only: EXIF/GPS metadata, ICC tricks, trailing
//!   payloads and appended archives never survive.
//! - Images are downscaled (avatar 320×320, banner within 1280×640) and compressed, which keeps the
//!   database small.

use std::io::{Cursor, Read, Write};

use base64::Engine as _;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits, Rgb, RgbImage};

use crate::db::sha256_bytes_hex;
use crate::error::ServerError;

/// Largest accepted upload (the base64 data URL string, before decoding).
pub const MAX_UPLOAD_DATA_URL_LEN: usize = 1_500_000;
const MAX_SOURCE_DIMENSION: u32 = 8_000;
const MAX_DECODE_ALLOC: u64 = 160 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SanitizedImage {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ZippedSanitizedImage {
    pub zip_bytes: Vec<u8>,
    pub raw_jpeg_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

/// Wraps a single file payload inside a standard Deflate-compressed PKZIP archive.
/// Pure safe Rust (no `unsafe`), verifiable by any standard `unzip` tool.
pub fn zip_single_file(entry_name: &str, data: &[u8]) -> Result<Vec<u8>, ServerError> {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    let crc32 = crc.sum();

    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|err| ServerError::internal(format!("ZIP deflate write failed: {err}")))?;
    let compressed = encoder
        .finish()
        .map_err(|err| ServerError::internal(format!("ZIP deflate finish failed: {err}")))?;

    let name_bytes = entry_name.as_bytes();
    let name_len = u16::try_from(name_bytes.len())
        .map_err(|_| ServerError::internal("ZIP entry name too long"))?;
    let comp_len = u32::try_from(compressed.len())
        .map_err(|_| ServerError::internal("ZIP compressed payload too large"))?;
    let uncomp_len = u32::try_from(data.len())
        .map_err(|_| ServerError::internal("ZIP uncompressed payload too large"))?;

    let mut out = Vec::with_capacity(30 + name_bytes.len() + compressed.len() + 46 + name_bytes.len() + 22);

    // Local file header (30 bytes + filename)
    let local_header_offset = out.len() as u32;
    out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes()); // PK\x03\x04
    out.extend_from_slice(&20_u16.to_le_bytes()); // version needed: 2.0
    out.extend_from_slice(&0_u16.to_le_bytes()); // flags
    out.extend_from_slice(&8_u16.to_le_bytes()); // compression method: 8 (Deflate)
    out.extend_from_slice(&0_u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0_u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc32.to_le_bytes());
    out.extend_from_slice(&comp_len.to_le_bytes());
    out.extend_from_slice(&uncomp_len.to_le_bytes());
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // extra len
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(&compressed);

    // Central directory file header (46 bytes + filename)
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&0x0201_4b50_u32.to_le_bytes()); // PK\x01\x02
    out.extend_from_slice(&20_u16.to_le_bytes()); // version made by
    out.extend_from_slice(&20_u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0_u16.to_le_bytes()); // flags
    out.extend_from_slice(&8_u16.to_le_bytes()); // compression: Deflate
    out.extend_from_slice(&0_u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0_u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc32.to_le_bytes());
    out.extend_from_slice(&comp_len.to_le_bytes());
    out.extend_from_slice(&uncomp_len.to_le_bytes());
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // extra len
    out.extend_from_slice(&0_u16.to_le_bytes()); // comment len
    out.extend_from_slice(&0_u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0_u16.to_le_bytes()); // internal attrs
    out.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
    out.extend_from_slice(&local_header_offset.to_le_bytes());
    out.extend_from_slice(name_bytes);
    let cd_size = (out.len() as u32).saturating_sub(cd_offset);

    // End of central directory record (22 bytes)
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes()); // PK\x05\x06
    out.extend_from_slice(&0_u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0_u16.to_le_bytes()); // cd start disk
    out.extend_from_slice(&1_u16.to_le_bytes()); // entries on disk
    out.extend_from_slice(&1_u16.to_le_bytes()); // total entries
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // comment len

    Ok(out)
}

/// Extracts the first entry from a single-file Deflate PKZIP archive (bounded to 8 MB).
pub fn unzip_single_file(zip_bytes: &[u8]) -> Result<Vec<u8>, ServerError> {
    if zip_bytes.len() < 30 || &zip_bytes[0..4] != b"PK\x03\x04" {
        return Err(ServerError::internal("Invalid ZIP archive header"));
    }
    let method = u16::from_le_bytes([zip_bytes[8], zip_bytes[9]]);
    let comp_len = u32::from_le_bytes([zip_bytes[18], zip_bytes[19], zip_bytes[20], zip_bytes[21]]) as usize;
    let uncomp_len = u32::from_le_bytes([zip_bytes[22], zip_bytes[23], zip_bytes[24], zip_bytes[25]]) as usize;
    let name_len = u16::from_le_bytes([zip_bytes[26], zip_bytes[27]]) as usize;
    let extra_len = u16::from_le_bytes([zip_bytes[28], zip_bytes[29]]) as usize;

    if uncomp_len > 8 * 1024 * 1024 {
        return Err(ServerError::internal("ZIP entry exceeds maximum allowed size"));
    }
    let data_start = 30usize
        .saturating_add(name_len)
        .saturating_add(extra_len);
    let data_end = data_start.saturating_add(comp_len);
    if data_end > zip_bytes.len() {
        return Err(ServerError::internal("Truncated ZIP archive"));
    }
    let payload = &zip_bytes[data_start..data_end];
    match method {
        0 => Ok(payload.to_vec()),
        8 => {
            let decoder = DeflateDecoder::new(payload);
            let mut out = Vec::with_capacity(uncomp_len.min(1024 * 1024));
            decoder
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut out)
                .map_err(|err| ServerError::internal(format!("ZIP inflate failed: {err}")))?;
            Ok(out)
        }
        _ => Err(ServerError::internal("Unsupported ZIP compression method")),
    }
}

/// Sanitizes an uploaded channel image (max 1280×1280 JPEG, EXIF/polyglot stripped)
/// and stores it inside a compressed PKZIP archive. CPU-bound: call from `spawn_blocking`.
pub fn sanitize_and_zip_channel_image(
    data_url: &str,
    label: &str,
    max_w: u32,
    max_h: u32,
    crop_square: bool,
    quality: u8,
) -> Result<ZippedSanitizedImage, ServerError> {
    let reject_img = |why: &str| {
        ServerError::with_user_message(
            nsac_core::error::ErrorCode::InvalidParams,
            format!("{label}: {why}"),
            format!("Rejected channel image ({label}): {why}"),
        )
    };

    if data_url.len() > MAX_UPLOAD_DATA_URL_LEN {
        return Err(reject_img("imagem muito grande (máx. ~1 MB)."));
    }
    let payload = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .filter(|(meta, _)| meta.ends_with(";base64") && meta.starts_with("image/"))
        .map(|(_, payload)| payload)
        .ok_or_else(|| reject_img("formato inválido."))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| reject_img("arquivo corrompido."))?;

    let format = image::guess_format(&bytes)
        .map_err(|_| reject_img("envie uma imagem PNG, JPEG, WebP ou GIF."))?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        return Err(reject_img("envie uma imagem PNG, JPEG, WebP ou GIF."));
    }

    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| reject_img("não foi possível ler a imagem."))?;
    if decoded.width() < 16 || decoded.height() < 16 {
        return Err(reject_img("imagem pequena demais."));
    }

    let resized = if crop_square {
        decoded.resize_to_fill(max_w, max_h, FilterType::Lanczos3)
    } else if decoded.width() > max_w || decoded.height() > max_h {
        decoded.resize(max_w, max_h, FilterType::Lanczos3)
    } else {
        decoded
    };
    let width = resized.width();
    let height = resized.height();
    let rgb = flatten(&resized);

    let mut jpeg = Vec::with_capacity(96 * 1024);
    JpegEncoder::new_with_quality(&mut jpeg, quality)
        .encode_image(&rgb)
        .map_err(|_| ServerError::internal("Channel image JPEG re-encode failed"))?;
    let sha256 = sha256_bytes_hex(&jpeg);
    let zip_bytes = zip_single_file("image.jpg", &jpeg)?;

    Ok(ZippedSanitizedImage {
        zip_bytes,
        raw_jpeg_bytes: jpeg,
        width,
        height,
        sha256,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileMediaKind {
    Avatar,
    Banner,
}

impl ProfileMediaKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Avatar => "Foto de perfil",
            Self::Banner => "Banner",
        }
    }
}

fn reject(kind: ProfileMediaKind, why: &str) -> ServerError {
    ServerError::with_user_message(
        nsac_core::error::ErrorCode::InvalidParams,
        format!("{}: {why}", kind.label()),
        format!("Rejected {kind:?} upload: {why}"),
    )
}

/// Validates, decodes, downsizes and re-encodes an uploaded `data:` URL.
/// Returns a new `data:image/jpeg;base64,…` URL. CPU-bound: call from `spawn_blocking`.
pub fn sanitize_profile_image(data_url: &str, kind: ProfileMediaKind) -> Result<String, ServerError> {
    if data_url.len() > MAX_UPLOAD_DATA_URL_LEN {
        return Err(reject(kind, "imagem muito grande (máx. ~1 MB)."));
    }
    let payload = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .filter(|(meta, _)| meta.ends_with(";base64") && meta.starts_with("image/"))
        .map(|(_, payload)| payload)
        .ok_or_else(|| reject(kind, "formato inválido."))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| reject(kind, "arquivo corrompido."))?;

    // Trust the bytes, not the declared type.
    let format = image::guess_format(&bytes).map_err(|_| reject(kind, "envie uma imagem PNG, JPEG, WebP ou GIF."))?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif) {
        return Err(reject(kind, "envie uma imagem PNG, JPEG, WebP ou GIF."));
    }

    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| reject(kind, "não foi possível ler a imagem."))?;
    if decoded.width() < 16 || decoded.height() < 16 {
        return Err(reject(kind, "imagem pequena demais."));
    }

    let resized = match kind {
        ProfileMediaKind::Avatar => decoded.resize_to_fill(320, 320, FilterType::Lanczos3),
        ProfileMediaKind::Banner => {
            if decoded.width() > 1280 || decoded.height() > 640 {
                decoded.resize(1280, 640, FilterType::Lanczos3)
            } else {
                decoded
            }
        }
    };
    let rgb = flatten(&resized);
    let quality = match kind {
        ProfileMediaKind::Avatar => 80,
        ProfileMediaKind::Banner => 72,
    };
    let mut out = Vec::with_capacity(64 * 1024);
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|_| ServerError::internal("JPEG re-encode failed"))?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&out)
    ))
}

/// Validates, decodes, downsizes (within 1280×1280) and re-encodes a bug-report screenshot
/// from raw pixels to fresh JPEG bytes (stripping EXIF/GPS/ICC/polyglots).
/// CPU-bound: call from `spawn_blocking`.
pub fn sanitize_feedback_screenshot(data_url: &str) -> Result<SanitizedImage, ServerError> {
    let reject_shot = |why: &str| {
        ServerError::with_user_message(
            nsac_core::error::ErrorCode::InvalidParams,
            format!("Print do bug: {why}"),
            format!("Rejected feedback screenshot upload: {why}"),
        )
    };

    if data_url.len() > MAX_UPLOAD_DATA_URL_LEN {
        return Err(reject_shot("imagem muito grande (máx. ~1 MB por print)."));
    }
    let payload = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .filter(|(meta, _)| meta.ends_with(";base64") && meta.starts_with("image/"))
        .map(|(_, payload)| payload)
        .ok_or_else(|| reject_shot("formato inválido."))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| reject_shot("arquivo corrompido."))?;

    let format = image::guess_format(&bytes)
        .map_err(|_| reject_shot("envie uma imagem PNG, JPEG, WebP ou GIF."))?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        return Err(reject_shot("envie uma imagem PNG, JPEG, WebP ou GIF."));
    }

    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| reject_shot("não foi possível ler a imagem."))?;
    if decoded.width() < 32 || decoded.height() < 32 {
        return Err(reject_shot("imagem pequena demais (mín. 32×32 px)."));
    }

    let resized = if decoded.width() > 1280 || decoded.height() > 1280 {
        decoded.resize(1280, 1280, FilterType::Lanczos3)
    } else {
        decoded
    };
    let width = resized.width();
    let height = resized.height();
    let rgb = flatten(&resized);

    let mut out = Vec::with_capacity(96 * 1024);
    JpegEncoder::new_with_quality(&mut out, 78)
        .encode_image(&rgb)
        .map_err(|_| ServerError::internal("Feedback screenshot JPEG re-encode failed"))?;
    let sha256 = sha256_bytes_hex(&out);

    Ok(SanitizedImage {
        bytes: out,
        width,
        height,
        sha256,
    })
}

/// JPEG has no alpha: transparent pixels are composited over a neutral dark gray.
fn flatten(img: &DynamicImage) -> RgbImage {
    let rgba = img.to_rgba8();
    let bg = [30u16, 30, 30];
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = u16::from(p[3]);
        let mix = |c: u8, b: u16| ((u16::from(c) * a + b * (255 - a)) / 255) as u8;
        out.put_pixel(x, y, Rgb([mix(p[0], bg[0]), mix(p[1], bg[1]), mix(p[2], bg[2])]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_data_url(w: u32, h: u32) -> String {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba([200, 10, 10, 128])));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png).unwrap();
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(buf))
    }

    #[test]
    fn reencodes_and_downsizes_real_images() {
        let out = sanitize_profile_image(&png_data_url(1000, 600), ProfileMediaKind::Avatar).unwrap();
        assert!(out.starts_with("data:image/jpeg;base64,"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(out.trim_start_matches("data:image/jpeg;base64,"))
            .unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (320, 320));

        let banner = sanitize_profile_image(&png_data_url(3000, 1000), ProfileMediaKind::Banner).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(banner.trim_start_matches("data:image/jpeg;base64,"))
            .unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!(img.width(), 1280);
    }

    #[test]
    fn rejects_non_images_svg_and_disguised_files() {
        let b64 = |s: &[u8]| base64::engine::general_purpose::STANDARD.encode(s);
        let svg = format!("data:image/svg+xml;base64,{}", b64(b"<svg onload=alert(1)></svg>"));
        assert!(sanitize_profile_image(&svg, ProfileMediaKind::Avatar).is_err());
        let exe = format!("data:image/png;base64,{}", b64(b"MZ\x90\x00\x03 this is an exe"));
        assert!(sanitize_profile_image(&exe, ProfileMediaKind::Avatar).is_err());
        let html = format!("data:image/jpeg;base64,{}", b64(b"<html><script>x</script>"));
        assert!(sanitize_profile_image(&html, ProfileMediaKind::Banner).is_err());
        assert!(sanitize_profile_image("data:text/html,<b>", ProfileMediaKind::Avatar).is_err());
        // PNG signature followed by garbage: guessed as PNG but fails to decode.
        let mut fake = b"\x89PNG\r\n\x1a\n".to_vec();
        fake.extend_from_slice(&[0u8; 64]);
        assert!(sanitize_profile_image(&format!("data:image/png;base64,{}", b64(&fake)), ProfileMediaKind::Avatar).is_err());
    }

    #[test]
    fn appended_payload_is_dropped() {
        let url = png_data_url(64, 64);
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(url.trim_start_matches("data:image/png;base64,"))
            .unwrap();
        bytes.extend_from_slice(b"PK\x03\x04 hidden zip <script>evil()</script>");
        let polyglot = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes));
        let out = sanitize_profile_image(&polyglot, ProfileMediaKind::Avatar).unwrap();
        let clean = base64::engine::general_purpose::STANDARD
            .decode(out.trim_start_matches("data:image/jpeg;base64,"))
            .unwrap();
        assert!(!clean.windows(6).any(|w| w == b"script"));
    }

    #[test]
    fn sanitizes_feedback_screenshots_and_rejects_tiny_or_invalid() {
        let shot = sanitize_feedback_screenshot(&png_data_url(2400, 1200)).unwrap();
        assert_eq!((shot.width, shot.height), (1280, 640));
        assert_eq!(shot.sha256.len(), 64);
        assert!(image::load_from_memory(&shot.bytes).is_ok());

        // Smaller than 32x32 is rejected for screenshots
        assert!(sanitize_feedback_screenshot(&png_data_url(24, 24)).is_err());
        assert!(sanitize_feedback_screenshot("data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=").is_err());
    }

    #[test]
    fn channel_images_are_sanitized_and_stored_as_valid_zip() {
        let zipped = sanitize_and_zip_channel_image(
            &png_data_url(1600, 900),
            "Anúncio",
            1280,
            1280,
            false,
            78,
        )
        .unwrap();
        assert_eq!((zipped.width, zipped.height), (1280, 720));
        assert!(zipped.zip_bytes.starts_with(b"PK\x03\x04"));
        let extracted = unzip_single_file(&zipped.zip_bytes).unwrap();
        assert_eq!(extracted, zipped.raw_jpeg_bytes);
        assert!(image::load_from_memory(&extracted).is_ok());
    }
}
