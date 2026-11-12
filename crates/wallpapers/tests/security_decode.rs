use omarchy_photo_wallpapers::imaging::decode;

#[test]
fn png_decompression_bomb_geometry_is_rejected_before_raster_allocation() {
    use image::ImageEncoder;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&[30, 60, 90], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    bytes[16..20].copy_from_slice(&100000u32.to_be_bytes());
    bytes[20..24].copy_from_slice(&100000u32.to_be_bytes());
    let mut crc = u32::MAX;
    for byte in &bytes[12..29] {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    bytes[29..33].copy_from_slice(&(!crc).to_be_bytes());
    assert_eq!(
        decode::decode(&bytes, 1).unwrap_err().to_string(),
        "image_pixel_limit"
    );
}

#[test]
fn truncated_and_unrecognized_media_fail_without_diagnostics_containing_input() {
    for bytes in [
        b"synthetic-private-image-metadata".as_slice(),
        b"\x89PNG\r\n\x1a\n",
        b"\xff\xd8\xff",
    ] {
        let error = decode::decode(bytes, 1).unwrap_err().to_string();
        assert!(!error.contains("synthetic-private-image-metadata"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn malformed_non_picture_handler_cannot_reach_item_data_enumeration() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut bytes = STANDARD
        .decode(include_str!("fixtures/tiny-heif.base64").trim())
        .unwrap();
    let position = bytes.windows(4).position(|w| w == b"pict").unwrap();
    bytes[position..position + 4].copy_from_slice(b"pIct");
    assert!(decode::decode(&bytes, 1).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn oversized_heif_tile_is_rejected_by_native_security_budget() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut bytes = STANDARD
        .decode(include_str!("fixtures/tiny-heif.base64").trim())
        .unwrap();
    let position = bytes.windows(4).position(|w| w == b"ispe").unwrap();
    bytes[position + 8..position + 12].copy_from_slice(&20000u32.to_be_bytes());
    bytes[position + 12..position + 16].copy_from_slice(&20000u32.to_be_bytes());
    // Tiny valid payload with an oversized tile declaration. Native security
    // limits must reject it before allocating the declared plane.
    assert_eq!(
        decode::decode(&bytes, 1).unwrap_err().to_string(),
        "image_security_limit"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn bounded_native_heif_parser_still_decodes_an_ordinary_image() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let bytes = STANDARD
        .decode(include_str!("fixtures/tiny-heif.base64").trim())
        .unwrap();
    let rgb = decode::decode(&bytes, 1).unwrap();
    assert_eq!(rgb.dimensions(), (8, 8));
}

#[cfg(target_os = "linux")]
#[test]
fn unused_broken_optional_codec_does_not_disable_builtin_heic_decoding() {
    const CHILD: &str = "PHOTO_WALLPAPERS_BROKEN_CODEC_TEST";
    if std::env::var_os(CHILD).is_some() {
        bounded_native_heif_parser_still_decodes_an_ordinary_image();
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    // An inert, invalid ELF file models an optional plugin with an unavailable
    // dependency. No external executable or plugin function is invoked.
    std::fs::write(
        directory.path().join("libheif-synthetic-unavailable.so"),
        b"inert synthetic invalid ELF",
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "unused_broken_optional_codec_does_not_disable_builtin_heic_decoding",
        ])
        .env(CHILD, "1")
        .env("LIBHEIF_PLUGIN_PATH", directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated optional-codec child failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn native_heif_security_floor_rejects_older_runtime_versions() {
    for version in [[1, 21, 0], [1, 23, 1], [1, 23, 2], [1, 23, 5], [1, 23, 6]] {
        assert_eq!(
            decode::require_heif_version(version)
                .unwrap_err()
                .to_string(),
            "native_heif_security_update_required"
        );
    }
    for version in [[1, 23, 7], [1, 24, 0], [2, 0, 0]] {
        decode::require_heif_version(version).unwrap();
    }
}
