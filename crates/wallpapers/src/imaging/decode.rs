use anyhow::{Result, ensure};
use image::{DynamicImage, ImageDecoder, RgbImage, metadata::Orientation};
use moxcms::{ColorProfile, Layout, TransformOptions};
use std::io::Cursor;
const PIXELS: u64 = 70_000_000;
/// Security floor includes upstream fixes through the 1.23.7 release. The
/// system library is dynamically linked, so the build version is insufficient.
pub fn require_heif_version(version: [u8; 3]) -> Result<()> {
    ensure!(
        version >= [1, 23, 7],
        "native_heif_security_update_required"
    );
    Ok(())
}
pub fn srgb(mut rgb: RgbImage, profile: Option<ColorProfile>) -> Result<RgbImage> {
    if let Some(profile) = profile {
        let transform = profile
            .create_transform_8bit(
                Layout::Rgb,
                &ColorProfile::new_srgb(),
                Layout::Rgb,
                TransformOptions::default(),
            )
            .map_err(|_| anyhow::anyhow!("unsupported_image_color_profile"))?;
        let mut row = vec![0; rgb.width() as usize * 3];
        for source in rgb.as_mut().chunks_exact_mut(row.len()) {
            transform
                .transform(source, &mut row)
                .map_err(|_| anyhow::anyhow!("image_color_conversion_failed"))?;
            source.copy_from_slice(&row);
        }
    }
    Ok(rgb)
}
pub fn decode(bytes: &[u8], orientation: u32) -> Result<RgbImage> {
    if bytes.get(4..8) == Some(b"ftyp") {
        return heif(bytes);
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| anyhow::anyhow!("unsupported_image_format"))?;
    let (w, h) = decoder.dimensions();
    ensure!(w as u64 * h as u64 <= PIXELS, "image_pixel_limit");
    let icc = decoder.icc_profile()?.filter(|p| !p.is_empty());
    let profile = if let Some(icc) = icc {
        ensure!(icc.len() <= 2 * 1024 * 1024, "image_profile_size_limit");
        Some(
            ColorProfile::new_from_slice(&icc)
                .map_err(|_| anyhow::anyhow!("invalid_image_color_profile"))?,
        )
    } else {
        None
    };
    let mut orient = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    if orient == Orientation::NoTransforms {
        orient = Orientation::from_exif(orientation as u8).unwrap_or(Orientation::NoTransforms);
    }
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|_| anyhow::anyhow!("invalid_image_data"))?;
    image.apply_orientation(orient);
    srgb(image.into_rgb8(), profile)
}
#[cfg(not(target_os = "linux"))]
fn heif(_bytes: &[u8]) -> Result<RgbImage> {
    anyhow::bail!("heif_decoding_available_on_linux")
}
#[cfg(target_os = "linux")]
fn heif(bytes: &[u8]) -> Result<RgbImage> {
    use anyhow::Context;
    use libheif_rs::{
        ColorPrimaries as P, ColorSpace, HeifContext, LibHeif, RgbChroma,
        TransferCharacteristics as T,
    };
    // new_checked requires every installed optional codec plugin to load,
    // including unused encoders. Normal initialization retains available
    // decoders; a missing required decoder still fails at bounded decode.
    let lib = LibHeif::new();
    require_heif_version(lib.version())?;
    let mut ctx = HeifContext::new().map_err(|_| anyhow::anyhow!("heif_decoder_unavailable"))?;
    // Apply the app's budget before parsing boxes, including tile dimensions
    // and metadata; checking only the primary image after parsing is too late.
    let mut limits = ctx.security_limits();
    limits.set_max_image_size_pixels(PIXELS);
    limits.set_max_memory_block_size(256 * 1024 * 1024);
    limits.set_max_total_memory(512 * 1024 * 1024);
    limits.set_max_color_profile_size(2 * 1024 * 1024);
    limits.set_max_number_of_tiles(4096);
    limits.set_max_items(16384);
    limits.set_max_components(4);
    ctx.set_security_limits(&limits)
        .map_err(|_| anyhow::anyhow!("heif_security_limits_unavailable"))?;
    ctx.read_bytes(bytes)
        .map_err(|_| anyhow::anyhow!("invalid_heif_data"))?;
    ctx.set_max_decoding_threads(1);
    let handle = ctx.primary_image_handle()?;
    ensure!(
        handle.width() as u64 * handle.height() as u64 <= PIXELS,
        "image_pixel_limit"
    );
    let decoded = lib
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None)
        .map_err(|error| {
            if matches!(
                error.sub_code,
                libheif_rs::HeifErrorSubCode::SecurityLimitExceeded
            ) {
                anyhow::anyhow!("image_security_limit")
            } else {
                anyhow::anyhow!("heif_decode_failed")
            }
        })?;
    let profile = if let Some(raw) = decoded.color_profile_raw() {
        ensure!(
            raw.data.len() <= 2 * 1024 * 1024,
            "image_profile_size_limit"
        );
        Some(
            ColorProfile::new_from_slice(&raw.data)
                .map_err(|_| anyhow::anyhow!("invalid_image_color_profile"))?,
        )
    } else if let Some(nclx) = decoded.color_profile_nclx() {
        ensure!(
            !matches!(
                nclx.transfer_characteristics(),
                T::ITU_R_BT_2100_0_PQ | T::ITU_R_BT_2100_0_HLG
            ),
            "unsupported_hdr_rendition"
        );
        match nclx.color_primaries() {
            P::SMPTE_EG_432_1 => Some(ColorProfile::new_display_p3()),
            P::ITU_R_BT_2020_2_and_2100_0 => Some(ColorProfile::new_bt2020()),
            P::ITU_R_BT_709_5 | P::Unspecified => None,
            _ => anyhow::bail!("unsupported_heif_color_profile"),
        }
    } else {
        None
    };
    let planes = decoded.planes();
    let plane = planes.interleaved.context("missing_heif_rgb_plane")?;
    let mut rgb = RgbImage::new(plane.width, plane.height);
    for (source, target) in plane
        .data
        .chunks_exact(plane.stride)
        .zip(rgb.as_mut().chunks_exact_mut(plane.width as usize * 3))
    {
        target.copy_from_slice(&source[..plane.width as usize * 3]);
    }
    // libheif applies HEIF irot/imir transforms; applying catalog EXIF again would rotate twice.
    srgb(rgb, profile)
}
