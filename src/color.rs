use std::collections::HashMap;

use image::{DynamicImage, GenericImageView};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    pub const fn packed(self) -> u32 {
        ((self.red as u32) << 16) | ((self.green as u32) << 8) | self.blue as u32
    }

    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.red, self.green, self.blue)
    }
}

#[derive(Debug, Error)]
pub enum ColorError {
    #[error("album artwork has no opaque, non-white pixels")]
    NoEligiblePixels,
}

#[derive(Default)]
struct Bucket {
    count: u32,
    red: u64,
    green: u64,
    blue: u64,
}

impl Bucket {
    fn add(&mut self, color: Rgb) {
        self.count += 1;
        self.red += u64::from(color.red);
        self.green += u64::from(color.green);
        self.blue += u64::from(color.blue);
    }

    fn average(&self) -> Rgb {
        let divisor = u64::from(self.count);
        Rgb::new(
            (self.red / divisor) as u8,
            (self.green / divisor) as u8,
            (self.blue / divisor) as u8,
        )
    }
}

pub fn ambient_color(image: &DynamicImage) -> Result<Rgb, ColorError> {
    let mut buckets = HashMap::<u16, Bucket>::new();

    for (_, _, pixel) in image.pixels() {
        let [red, green, blue, alpha] = pixel.0;
        if alpha < 125 || (red > 250 && green > 250 && blue > 250) {
            continue;
        }

        let key = (u16::from(red >> 4) << 8) | (u16::from(green >> 4) << 4) | u16::from(blue >> 4);
        buckets
            .entry(key)
            .or_default()
            .add(Rgb::new(red, green, blue));
    }

    let dominant = buckets
        .values()
        .max_by(|left, right| {
            bucket_score(left)
                .total_cmp(&bucket_score(right))
                .then_with(|| left.count.cmp(&right.count))
        })
        .ok_or(ColorError::NoEligiblePixels)?
        .average();

    Ok(boost(dominant, 1.5, 1.2))
}

fn bucket_score(bucket: &Bucket) -> f32 {
    let color = bucket.average();
    let (_, saturation, value) = rgb_to_hsv(color);
    bucket.count as f32 * (0.25 + 0.75 * saturation) * (0.4 + 0.6 * value)
}

fn boost(color: Rgb, saturation_multiplier: f32, brightness_multiplier: f32) -> Rgb {
    let (hue, saturation, value) = rgb_to_hsv(color);
    hsv_to_rgb(
        hue,
        (saturation * saturation_multiplier).min(1.0),
        (value * brightness_multiplier).min(1.0),
    )
}

fn rgb_to_hsv(color: Rgb) -> (f32, f32, f32) {
    let red = f32::from(color.red) / 255.0;
    let green = f32::from(color.green) / 255.0;
    let blue = f32::from(color.blue) / 255.0;
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let delta = maximum - minimum;

    let mut hue = if delta == 0.0 {
        0.0
    } else if maximum == red {
        60.0 * ((green - blue) / delta).rem_euclid(6.0)
    } else if maximum == green {
        60.0 * (((blue - red) / delta) + 2.0)
    } else {
        60.0 * (((red - green) / delta) + 4.0)
    };
    if hue < 0.0 {
        hue += 360.0;
    }

    let saturation = if maximum == 0.0 { 0.0 } else { delta / maximum };
    (hue, saturation, maximum)
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> Rgb {
    let chroma = value * saturation;
    let intermediate = chroma * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let offset = value - chroma;
    let (red, green, blue) = match hue {
        h if h < 60.0 => (chroma, intermediate, 0.0),
        h if h < 120.0 => (intermediate, chroma, 0.0),
        h if h < 180.0 => (0.0, chroma, intermediate),
        h if h < 240.0 => (0.0, intermediate, chroma),
        h if h < 300.0 => (intermediate, 0.0, chroma),
        _ => (chroma, 0.0, intermediate),
    };

    Rgb::new(
        ((red + offset) * 255.0).round() as u8,
        ((green + offset) * 255.0).round() as u8,
        ((blue + offset) * 255.0).round() as u8,
    )
}
