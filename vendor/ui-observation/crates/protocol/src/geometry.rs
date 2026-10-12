use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

/// A protocol decimal stored as signed millionths and serialized with exactly
/// six fractional digits. No floating-point number crosses the wire.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Fixed6(i64);

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum GeometryError {
    #[error("NONCANONICAL_DECIMAL: expected an optional minus, digits, '.', and six digits")]
    NoncanonicalDecimal,
    #[error("DECIMAL_OUT_OF_RANGE")]
    OutOfRange,
    #[error("FRACTION_OUT_OF_RANGE")]
    FractionOutOfRange,
    #[error("RECT_NOT_NORMALIZED")]
    RectNotNormalized,
    #[error("NONFINITE_GEOMETRY")]
    Nonfinite,
}

impl Fixed6 {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(1_000_000);

    #[must_use]
    pub const fn from_millionths(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn millionths(self) -> i64 {
        self.0
    }

    pub fn checked_from_f32(value: f32) -> Result<Self, GeometryError> {
        if !value.is_finite() {
            return Err(GeometryError::Nonfinite);
        }
        let scaled = f64::from(value) * 1_000_000.0;
        if scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
            return Err(GeometryError::OutOfRange);
        }
        Ok(Self(scaled.round() as i64))
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }
}

impl fmt::Display for Fixed6 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let negative = self.0 < 0;
        let magnitude = self.0.unsigned_abs();
        if negative {
            write!(f, "-")?;
        }
        write!(f, "{}.{:06}", magnitude / 1_000_000, magnitude % 1_000_000)
    }
}

impl FromStr for Fixed6 {
    type Err = GeometryError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (negative, unsigned) = input
            .strip_prefix('-')
            .map_or((false, input), |s| (true, s));
        let (whole, fraction) = unsigned
            .split_once('.')
            .ok_or(GeometryError::NoncanonicalDecimal)?;
        if whole.is_empty()
            || fraction.len() != 6
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || (whole.len() > 1 && whole.starts_with('0'))
            || (negative && whole == "0" && fraction == "000000")
        {
            return Err(GeometryError::NoncanonicalDecimal);
        }
        let whole = whole
            .parse::<u64>()
            .map_err(|_| GeometryError::OutOfRange)?;
        let fraction = fraction
            .parse::<u64>()
            .map_err(|_| GeometryError::OutOfRange)?;
        let magnitude = whole
            .checked_mul(1_000_000)
            .and_then(|v| v.checked_add(fraction))
            .ok_or(GeometryError::OutOfRange)?;
        let value = if negative {
            if magnitude == (i64::MAX as u64) + 1 {
                i64::MIN
            } else {
                -i64::try_from(magnitude).map_err(|_| GeometryError::OutOfRange)?
            }
        } else {
            i64::try_from(magnitude).map_err(|_| GeometryError::OutOfRange)?
        };
        Ok(Self(value))
    }
}

impl Serialize for Fixed6 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Fixed6 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fraction(Fixed6);

impl Fraction {
    pub const ZERO: Self = Self(Fixed6::ZERO);
    pub const ONE: Self = Self(Fixed6::ONE);

    pub fn new(value: Fixed6) -> Result<Self, GeometryError> {
        if (0..=1_000_000).contains(&value.0) {
            Ok(Self(value))
        } else {
            Err(GeometryError::FractionOutOfRange)
        }
    }

    #[must_use]
    pub const fn fixed(self) -> Fixed6 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub min_x: Fixed6,
    pub min_y: Fixed6,
    pub max_x: Fixed6,
    pub max_y: Fixed6,
}

impl Rect {
    pub fn new(
        min_x: Fixed6,
        min_y: Fixed6,
        max_x: Fixed6,
        max_y: Fixed6,
    ) -> Result<Self, GeometryError> {
        if max_x < min_x || max_y < min_y {
            return Err(GeometryError::RectNotNormalized);
        }
        Ok(Self {
            min_x,
            min_y,
            max_x,
            max_y,
        })
    }

    pub fn from_f32(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Result<Self, GeometryError> {
        Self::new(
            Fixed6::checked_from_f32(min_x)?,
            Fixed6::checked_from_f32(min_y)?,
            Fixed6::checked_from_f32(max_x)?,
            Fixed6::checked_from_f32(max_y)?,
        )
    }

    #[must_use]
    pub fn intersection(self, other: Self) -> Self {
        let min_x = self.min_x.max(other.min_x);
        let min_y = self.min_y.max(other.min_y);
        let max_x = self.max_x.min(other.max_x).max(min_x);
        let max_y = self.max_y.min(other.max_y).max(min_y);
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    #[must_use]
    pub fn intersect_all(rects: &[Self]) -> Option<Self> {
        rects.iter().copied().reduce(Self::intersection)
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.min_x == self.max_x || self.min_y == self.max_y
    }

    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        self.min_x <= other.min_x
            && self.min_y <= other.min_y
            && self.max_x >= other.max_x
            && self.max_y >= other.max_y
    }

    pub fn visible_fraction(self, visible: Self) -> Result<Fraction, GeometryError> {
        let width = self
            .max_x
            .checked_sub(self.min_x)
            .ok_or(GeometryError::OutOfRange)?
            .0;
        let height = self
            .max_y
            .checked_sub(self.min_y)
            .ok_or(GeometryError::OutOfRange)?
            .0;
        if width == 0 || height == 0 {
            return Ok(Fraction::ZERO);
        }
        let visible_width = visible
            .max_x
            .checked_sub(visible.min_x)
            .ok_or(GeometryError::OutOfRange)?
            .0;
        let visible_height = visible
            .max_y
            .checked_sub(visible.min_y)
            .ok_or(GeometryError::OutOfRange)?
            .0;
        let numerator = i128::from(visible_width) * i128::from(visible_height) * 1_000_000;
        let denominator = i128::from(width) * i128::from(height);
        let rounded = (numerator + denominator / 2) / denominator;
        let bounded = rounded.clamp(0, 1_000_000);
        Fraction::new(Fixed6::from_millionths(
            i64::try_from(bounded).map_err(|_| GeometryError::OutOfRange)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_is_exact_and_canonical() {
        for text in [
            "0.000000",
            "1.234567",
            "-1.234567",
            "9223372036854.775807",
            "-9223372036854.775808",
        ] {
            let value: Fixed6 = text.parse().unwrap();
            assert_eq!(value.to_string(), text);
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{text}\"")
            );
        }
        for bad in [
            "0",
            "0.0",
            "+1.000000",
            "01.000000",
            "-0.000000",
            "NaN",
            "inf",
            "9223372036854.775808",
        ] {
            assert!(bad.parse::<Fixed6>().is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn intersection_and_fraction_are_mechanical() {
        let layout = Rect::from_f32(0.0, 0.0, 100.0, 20.0).unwrap();
        let clip = Rect::from_f32(25.0, -10.0, 75.0, 10.0).unwrap();
        let visible = layout.intersection(clip);
        assert_eq!(visible, Rect::from_f32(25.0, 0.0, 75.0, 10.0).unwrap());
        assert_eq!(
            layout
                .visible_fraction(visible)
                .unwrap()
                .fixed()
                .to_string(),
            "0.250000"
        );
    }
}
