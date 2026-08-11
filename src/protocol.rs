use std::{fmt, str::FromStr};

use serde::Serialize;
use thiserror::Error;

pub const REPORT_SIZE: usize = 64;
const REPORT_ID: u8 = 0x06;
const RPC_CHANNEL: u8 = 0x02;
const MAX_CHUNK_SIZE: usize = 61;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("{0} must be a finite value between 0 and 1")]
    InvalidUnitInterval(&'static str),
    #[error("could not encode Codex Micro request: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum LightingEffect {
    Off = 0,
    Solid = 1,
    Snake = 2,
    Rainbow = 3,
    Breath = 4,
    Gradient = 5,
    ShallowBreath = 6,
}

impl LightingEffect {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Solid,
        Self::Snake,
        Self::Rainbow,
        Self::Breath,
        Self::Gradient,
        Self::ShallowBreath,
    ];

    pub const fn code(self) -> u8 {
        self as u8
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Solid => "solid",
            Self::Snake => "snake",
            Self::Rainbow => "rainbow",
            Self::Breath => "breath",
            Self::Gradient => "gradient",
            Self::ShallowBreath => "shallow-breath",
        }
    }
}

impl fmt::Display for LightingEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Error)]
#[error("unknown lighting effect '{0}'; expected off, solid, snake, rainbow, breath, gradient, or shallow-breath")]
pub struct ParseLightingEffectError(String);

impl FromStr for LightingEffect {
    type Err = ParseLightingEffectError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "solid" => Ok(Self::Solid),
            "snake" => Ok(Self::Snake),
            "rainbow" => Ok(Self::Rainbow),
            "breath" => Ok(Self::Breath),
            "gradient" => Ok(Self::Gradient),
            "shallow-breath" | "shallowbreath" | "shallow_breath" => Ok(Self::ShallowBreath),
            _ => Err(ParseLightingEffectError(value.to_owned())),
        }
    }
}

#[derive(Serialize)]
pub struct RpcRequest {
    method: &'static str,
    params: LightingConfig,
    id: u16,
}

#[derive(Serialize)]
struct LightingConfig {
    keys: LightingSide,
    ambient: LightingSide,
}

#[derive(Serialize)]
struct LightingSide {
    e: u8,
    b: f32,
    s: f32,
    m: f32,
    c: u32,
}

impl LightingSide {
    const fn off() -> Self {
        Self {
            e: 0,
            b: 0.0,
            s: 0.0,
            m: 0.0,
            c: 0,
        }
    }

    const fn effect(
        effect: LightingEffect,
        color: u32,
        brightness: f32,
        speed: f32,
        magic: f32,
    ) -> Self {
        Self {
            e: effect.code(),
            b: brightness,
            s: speed,
            m: magic,
            c: color,
        }
    }
}

pub fn ambient_effect_request(
    id: u16,
    effect: LightingEffect,
    packed_rgb: u32,
    brightness: f32,
    speed: f32,
    magic: f32,
) -> Result<RpcRequest, ProtocolError> {
    validate_unit_interval("brightness", brightness)?;
    validate_unit_interval("speed", speed)?;
    validate_unit_interval("magic", magic)?;

    Ok(RpcRequest {
        method: "v.oai.rgbcfg",
        params: LightingConfig {
            keys: LightingSide::off(),
            ambient: LightingSide::effect(
                effect,
                packed_rgb & 0x00FF_FFFF,
                brightness,
                speed,
                magic,
            ),
        },
        id,
    })
}

fn validate_unit_interval(name: &'static str, value: f32) -> Result<(), ProtocolError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(ProtocolError::InvalidUnitInterval(name));
    }
    Ok(())
}

pub fn off_request(id: u16) -> RpcRequest {
    RpcRequest {
        method: "v.oai.rgbcfg",
        params: LightingConfig {
            keys: LightingSide::off(),
            ambient: LightingSide::off(),
        },
        id,
    }
}

pub fn frame_rpc_request(request: &RpcRequest) -> Result<Vec<[u8; REPORT_SIZE]>, ProtocolError> {
    let message = serde_json::to_vec(request)?;
    let mut reports = Vec::with_capacity(message.len().div_ceil(MAX_CHUNK_SIZE));

    for chunk in message.chunks(MAX_CHUNK_SIZE) {
        let mut report = [0_u8; REPORT_SIZE];
        report[0] = REPORT_ID;
        report[1] = RPC_CHANNEL;
        report[2] = chunk.len() as u8;
        report[3..3 + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }

    Ok(reports)
}
