// SPDX-License-Identifier: MIT OR Apache-2.0
use std::{fmt, str::FromStr};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{AlefError, ErrorCode};

/// Pixel or percentage geometry unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthUnit {
    /// Percentage of the complete screen.
    Screen,
    /// Percentage of the screen work area.
    Work,
}

/// A non-negative pixel length or bounded percentage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// Pixel length.
    Px(f64),
    /// Percentage with its screen-relative unit.
    Percent(f64, LengthUnit),
}

impl Length {
    fn checked(value: f64) -> Result<f64, AlefError> {
        if value.is_finite() && value >= 0.0 {
            Ok(value)
        } else {
            Err(AlefError::new(ErrorCode::ManifestInvalid, "invalid length"))
        }
    }
}

impl FromStr for Length {
    type Err = AlefError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some((number, unit)) = value.split_once('%') {
            if number.trim() != number || number.is_empty() {
                return Err(AlefError::new(ErrorCode::ManifestInvalid, "invalid length"));
            }
            let number = number
                .parse::<f64>()
                .map_err(|_| AlefError::new(ErrorCode::ManifestInvalid, "invalid length"))?;
            if !number.is_finite() || !(1.0..=100.0).contains(&number) {
                return Err(AlefError::new(ErrorCode::ManifestInvalid, "invalid length"));
            }
            let unit = match unit {
                "screen" => LengthUnit::Screen,
                "work" => LengthUnit::Work,
                _ => return Err(AlefError::new(ErrorCode::ManifestInvalid, "invalid length")),
            };
            Ok(Self::Percent(number, unit))
        } else {
            let number = value
                .parse::<f64>()
                .map_err(|_| AlefError::new(ErrorCode::ManifestInvalid, "invalid length"))?;
            Ok(Self::Px(Self::checked(number)?))
        }
    }
}

impl fmt::Display for Length {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (number, suffix) = match self {
            Self::Px(number) => (*number, String::new()),
            Self::Percent(number, unit) => (
                *number,
                format!(
                    "%{}",
                    if *unit == LengthUnit::Work {
                        "work"
                    } else {
                        "screen"
                    }
                ),
            ),
        };
        if number.fract() == 0.0 {
            write!(formatter, "{number:.0}{suffix}")
        } else {
            write!(formatter, "{number}{suffix}")
        }
    }
}

impl Serialize for Length {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Length {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LengthVisitor;
        impl<'de> de::Visitor<'de> for LengthVisitor {
            type Value = Length;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a pixel number or percentage length")
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Length, E> {
                Length::checked(value).map(Length::Px).map_err(E::custom)
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Length, E> {
                self.visit_f64(value as f64)
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Length, E> {
                self.visit_f64(value as f64)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Length, E> {
                value.parse().map_err(E::custom)
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Length, E> {
                self.visit_str(&value)
            }
        }
        deserializer.deserialize_any(LengthVisitor)
    }
}

/// TypeScript representation accepts number|string input and outputs string.
impl ts_rs::TS for Length {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn docs() -> Option<String> {
        Some(ts_rs::format_docs(&[
            " Pixels or a percentage of a screen (for example `\"70%work\"`). A number (px) or a string is accepted on input; serialized values are always strings.",
        ]))
    }

    fn name(_cfg: &ts_rs::Config) -> String {
        "Length".to_owned()
    }

    fn inline(_cfg: &ts_rs::Config) -> String {
        "number | string".to_owned()
    }

    fn decl(cfg: &ts_rs::Config) -> String {
        format!("type {} = number | string;", Self::name(cfg))
    }

    fn output_path() -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from("manifest.ts"))
    }
}

/// Window monitor selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "manifest.ts")]
pub enum Monitor {
    /// The primary monitor; used when omitted.
    #[default]
    Primary,
    /// The monitor containing the cursor.
    Cursor,
}

/// Window placement: the string `center` or an `{x, y}` object (same shape on the way out).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum WindowPosition {
    /// Center the window.
    #[default]
    Center,
    /// Position the window at the specified coordinates.
    At {
        /// Horizontal coordinate.
        x: Length,
        /// Vertical coordinate.
        y: Length,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Xy {
    x: Length,
    y: Length,
}

impl Serialize for WindowPosition {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Center => serializer.serialize_str("center"),
            Self::At { x, y } => Xy { x: *x, y: *y }.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for WindowPosition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Position {
            Name(String),
            Xy(Xy),
        }
        match Position::deserialize(deserializer)? {
            Position::Name(value) if value == "center" => Ok(Self::Center),
            Position::Name(_) => Err(de::Error::custom("position must be center or {x,y}")),
            Position::Xy(Xy { x, y }) => Ok(Self::At { x, y }),
        }
    }
}

/// TypeScript representation of the center literal or coordinate object.
impl ts_rs::TS for WindowPosition {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn docs() -> Option<String> {
        Some(ts_rs::format_docs(&[
            " The string `center` or explicit coordinates.",
        ]))
    }

    fn name(_cfg: &ts_rs::Config) -> String {
        "WindowPosition".to_owned()
    }

    fn inline(_cfg: &ts_rs::Config) -> String {
        "\"center\" | { x: Length, y: Length }".to_owned()
    }

    fn decl(cfg: &ts_rs::Config) -> String {
        format!("type {} = {};", Self::name(cfg), Self::inline(cfg))
    }

    fn output_path() -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from("manifest.ts"))
    }
}

/// A declared application window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct WindowDef {
    /// Stable window label.
    pub label: String,
    /// Root-relative URL.
    pub url: String,
    /// Window width.
    pub width: Length,
    /// Window height.
    pub height: Length,
    /// Optional minimum width.
    #[ts(optional)]
    pub min_width: Option<Length>,
    /// Optional minimum height.
    #[ts(optional)]
    pub min_height: Option<Length>,
    /// Optional maximum width.
    #[ts(optional)]
    pub max_width: Option<Length>,
    /// Optional maximum height.
    #[ts(optional)]
    pub max_height: Option<Length>,
    /// Monitor selection.
    #[serde(default)]
    pub monitor: Monitor,
    /// Window placement.
    #[serde(default)]
    pub position: WindowPosition,
    /// Restore the window.
    #[serde(default)]
    pub restore: bool,
}
