// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure pixel conversions the system backends use: device formats in, RGBA out. No hardware, no
//! allocation beyond the returned buffer.
/// The largest frame edge accepted; anything larger is refused so the size math stays bounded.
const MAX_DIMENSION: u32 = 16_384;

fn frame_pixels(width: u32, height: u32) -> Option<u64> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return None;
    }
    Some(u64::from(width) * u64::from(height))
}

/// Reorders an `RGB8` buffer to `RGBA` with an opaque alpha; `None` unless the length matches
/// `width * height * 3`.
pub fn rgba_from_rgb(src: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let pixels = frame_pixels(width, height)?;
    if src.len() as u64 != pixels * 3 {
        return None;
    }
    let out_len = usize::try_from(pixels * 4).ok()?;
    let mut out = Vec::with_capacity(out_len);
    for px in src.chunks_exact(3) {
        out.extend_from_slice(px);
        out.push(255);
    }
    Some(out)
}

/// Expands a single-channel `GRAY8` buffer to `RGBA`; `None` unless the length matches
/// `width * height`.
pub fn rgba_from_gray(src: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let pixels = frame_pixels(width, height)?;
    if src.len() as u64 != pixels {
        return None;
    }
    let out_len = usize::try_from(pixels * 4).ok()?;
    let mut out = Vec::with_capacity(out_len);
    for &y in src {
        out.extend_from_slice(&[y, y, y, 255]);
    }
    Some(out)
}

/// Converts packed `YUYV422` (Y0 U Y1 V per four bytes) to `RGBA` with the BT.601 studio-swing
/// matrix; `None` unless the length matches `width * height * 2` and the width is even.
pub fn rgba_from_yuyv(src: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    if !width.is_multiple_of(2) {
        return None;
    }
    let pixels = frame_pixels(width, height)?;
    if src.len() as u64 != pixels * 2 {
        return None;
    }
    let out_len = usize::try_from(pixels * 4).ok()?;
    let mut out = Vec::with_capacity(out_len);
    for pair in src.chunks_exact(4) {
        let (y0, u, y1, v) = (pair[0], pair[1], pair[2], pair[3]);
        for y in [y0, y1] {
            out.extend_from_slice(&yuv_to_rgba(y, u, v));
        }
    }
    Some(out)
}

/// Converts an `NV12` frame (a full-resolution Y plane followed by quarter-resolution
/// interleaved UV, U first) to `RGBA`; `None` unless the width and height are even and the
/// length matches `width * height * 3 / 2`. Both pixels of each 2x2 block share one chroma.
pub fn rgba_from_nv12(src: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return None;
    }
    let pixels = frame_pixels(width, height)?;
    if src.len() as u64 != pixels + pixels / 2 {
        return None;
    }
    let out_len = usize::try_from(pixels * 4).ok()?;
    let chroma_stride = usize::try_from(width).ok()?;
    let luma_stride = usize::try_from(width).ok()?;
    let mut out = Vec::with_capacity(out_len);
    for row in 0..usize::try_from(height).ok()? {
        let chroma_row = row / 2;
        for column in 0..luma_stride {
            let chroma = chroma_row * chroma_stride + (column / 2) * 2;
            out.extend_from_slice(&yuv_to_rgba(
                src[row * luma_stride + column],
                src[pixels as usize + chroma],
                src[pixels as usize + chroma + 1],
            ));
        }
    }
    Some(out)
}

/// Reverses a packed `BGR8` buffer to `RGBA` with an opaque alpha; `None` unless the length
/// matches `width * height * 3`.
pub fn rgba_from_bgr(src: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let pixels = frame_pixels(width, height)?;
    if src.len() as u64 != pixels * 3 {
        return None;
    }
    let out_len = usize::try_from(pixels * 4).ok()?;
    let mut out = Vec::with_capacity(out_len);
    for px in src.chunks_exact(3) {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    Some(out)
}

/// The shared BT.601 studio-swing matrix: one pixel per call, opaque alpha.
fn yuv_to_rgba(y: u8, u: u8, v: u8) -> [u8; 4] {
    let yf = f32::from(y) - 16.0;
    let uf = f32::from(u) - 128.0;
    let vf = f32::from(v) - 128.0;
    let r = 1.164 * yf + 1.596 * vf;
    let g = 1.164 * yf - 0.391 * uf - 0.813 * vf;
    let b = 1.164 * yf + 2.018 * uf;
    [clamp_u8(r), clamp_u8(g), clamp_u8(b), 255]
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_becomes_rgba_with_opaque_alpha() {
        let out = rgba_from_rgb(&[1, 2, 3, 4, 5, 6], 2, 1).expect("a whole rgb buffer converts");
        assert_eq!(out, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn a_short_rgb_buffer_is_refused() {
        assert_eq!(rgba_from_rgb(&[1, 2, 3], 2, 1), None);
    }

    #[test]
    fn nv12_uses_full_width_chroma_stride_for_four_by_four() {
        // Two independent UV rows: red upper half, blue lower half.
        let input = [
            81, 81, 81, 81, 81, 81, 81, 81, 41, 41, 41, 41, 41, 41, 41, 41, 90, 240, 90, 240, 240,
            110, 240, 110,
        ];
        let out = rgba_from_nv12(&input, 4, 4).expect("valid NV12 frame");
        let expected = [
            254, 0, 0, 255, 254, 0, 0, 255, 254, 0, 0, 255, 254, 0, 0, 255, 254, 0, 0, 255, 254, 0,
            0, 255, 254, 0, 0, 255, 254, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
            0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn a_zero_sized_frame_is_refused_by_every_converter() {
        // Kills the removal of the `width == 0 || height == 0` guard in `frame_pixels`.
        assert_eq!(rgba_from_rgb(&[], 0, 0), None);
        assert_eq!(rgba_from_gray(&[], 0, 0), None);
        assert_eq!(rgba_from_yuyv(&[], 0, 0), None);
        assert_eq!(rgba_from_nv12(&[], 0, 0), None);
        assert_eq!(rgba_from_bgr(&[], 0, 0), None);
    }

    #[test]
    fn an_overlarge_frame_is_refused_without_panicking_in_every_converter() {
        // Kills the removal of `MAX_DIMENSION` or of the checked `u64` size arithmetic: the
        // `u32::MAX` product must be rejected before any `usize` conversion or allocation.
        assert_eq!(rgba_from_rgb(&[], u32::MAX, u32::MAX), None);
        assert_eq!(rgba_from_gray(&[], u32::MAX, u32::MAX), None);
        assert_eq!(rgba_from_yuyv(&[], u32::MAX, u32::MAX), None);
        assert_eq!(rgba_from_nv12(&[], u32::MAX, u32::MAX), None);
        assert_eq!(rgba_from_bgr(&[], u32::MAX, u32::MAX), None);
    }

    #[test]
    fn gray_expands_to_equal_channels_with_opaque_alpha() {
        let out = rgba_from_gray(&[7, 200], 2, 1).expect("a whole gray buffer converts");
        assert_eq!(out, vec![7, 7, 7, 255, 200, 200, 200, 255]);
        assert_eq!(rgba_from_gray(&[7], 2, 1), None);
    }

    #[test]
    fn yuyv_converts_two_pixels_per_four_bytes_with_neutral_chroma() {
        // The fixture is four bytes for a 2x1 image: Y=235 near white and Y=16 black, both with
        // U=V=128; 1.164 * (235 - 16) = 254.916 rounds to 255 and 1.164 * (16 - 16) = 0.
        let out = rgba_from_yuyv(&[235, 128, 16, 128], 2, 1).expect("a whole yuyv buffer converts");
        assert_eq!(out, vec![255, 255, 255, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn yuyv_converts_saturated_chroma_within_tolerance() {
        // Red pixel: Y=81, U=90, V=240 -> yf=65, uf=-38, vf=112.
        //   r = 1.164 * 65 + 1.596 * 112 = 75.66 + 178.752 = 254.412 -> 254
        //   g = 1.164 * 65 - 0.391 * (-38) - 0.813 * 112 = 75.66 + 14.858 - 91.056 = -0.538 -> 0
        //   b = 1.164 * 65 + 2.018 * (-38) = 75.66 - 76.684 = -1.024 -> 0
        // Blue pixel: Y=41, U=240, V=110 -> yf=25, uf=112, vf=-18.
        //   r = 1.164 * 25 + 1.596 * (-18) = 29.1 - 28.728 = 0.372 -> 0
        //   g = 1.164 * 25 - 0.391 * 112 - 0.813 * (-18) = 29.1 - 43.792 + 14.634 = -0.058 -> 0
        //   b = 1.164 * 25 + 2.018 * 112 = 29.1 + 226.016 = 255.116 -> 255
        // A 2x2 frame: the top row is two red pixels, the bottom row two blue pixels.
        let out = rgba_from_yuyv(&[81, 90, 81, 240, 41, 240, 41, 110], 2, 2)
            .expect("a whole yuyv buffer converts");
        // The floating-point BT.601 studio-swing matrix rounds saturated red to 254 and blue to 255.
        let expected = [254u8, 0, 0, 254, 0, 0, 0, 0, 255, 0, 0, 255];
        for (pixel, expected) in out.chunks_exact(4).zip(expected.chunks_exact(3).cycle()) {
            for (channel, expected) in pixel[..3].iter().zip(expected) {
                assert!(
                    i32::from(*channel).abs_diff(i32::from(*expected)) <= 2,
                    "channel {channel} deviates from {expected}"
                );
            }
            assert_eq!(pixel[3], 255);
        }
    }

    #[test]
    fn an_odd_or_short_yuyv_buffer_is_refused() {
        assert_eq!(rgba_from_yuyv(&[0; 8], 3, 1), None);
        assert_eq!(rgba_from_yuyv(&[0; 3], 2, 1), None);
    }

    #[test]
    fn nv12_shares_chroma_across_each_two_by_two_block() {
        // 2x2 frame: four Y bytes (white top row, black bottom row) plus one U and one V byte
        // for the single 2x2 chroma block; with U=V=128 the matrix reduces to 1.164 * (Y - 16).
        let out = rgba_from_nv12(&[235, 235, 16, 16, 128, 128], 2, 2)
            .expect("a whole nv12 buffer converts");
        assert_eq!(
            out,
            vec![255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255,]
        );
        // Kills a mutation of the `pixels + pixels / 2` NV12 length check.
        assert_eq!(rgba_from_nv12(&[235; 5], 2, 2), None);
        // Kills the removal of the even-width or even-height NV12 requirement.
        assert_eq!(rgba_from_nv12(&[0; 6], 3, 2), None);
        assert_eq!(rgba_from_nv12(&[0; 6], 2, 3), None);
    }

    #[test]
    fn bgr_is_reversed_to_rgb_with_opaque_alpha() {
        let out = rgba_from_bgr(&[3, 2, 1, 6, 5, 4], 2, 1).expect("a whole bgr buffer converts");
        assert_eq!(out, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        // Kills a mutation of the BGR length check.
        assert_eq!(rgba_from_bgr(&[3, 2, 1], 2, 1), None);
    }
}
