use codex_micro_chroma::color::{ambient_color, Rgb};
use image::{DynamicImage, Rgba, RgbaImage};

#[test]
fn chooses_the_dominant_non_white_opaque_color() {
    let mut image = RgbaImage::from_pixel(10, 10, Rgba([255, 255, 255, 255]));
    for x in 0..7 {
        for y in 0..10 {
            image.put_pixel(x, y, Rgba([180, 25, 30, 255]));
        }
    }

    let color = ambient_color(&DynamicImage::ImageRgba8(image)).unwrap();

    assert!(color.red > color.green * 3);
    assert!(color.red > color.blue * 3);
    assert_eq!(color.to_hex(), "#D80007");
}

#[test]
fn ignores_transparent_pixels() {
    let mut image = RgbaImage::from_pixel(4, 4, Rgba([0, 255, 0, 0]));
    image.put_pixel(0, 0, Rgba([20, 40, 180, 255]));

    let color = ambient_color(&DynamicImage::ImageRgba8(image)).unwrap();

    assert!(color.blue > color.red);
    assert!(color.blue > color.green);
}

#[test]
fn packed_rgb_matches_hardware_format() {
    assert_eq!(Rgb::new(0x12, 0x34, 0x56).packed(), 0x123456);
}
