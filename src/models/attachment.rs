use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_PIECE_STARS: u8 = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CurrencyId {
    Coins,
    Diamonds,
}

impl CurrencyId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coins => "coins",
            Self::Diamonds => "diamonds",
        }
    }
}

/// Piece kinds accepted today. Add a variant and a match arm to support a new kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceType {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl PieceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pawn => "pawn",
            Self::Knight => "knight",
            Self::Bishop => "bishop",
            Self::Rook => "rook",
            Self::Queen => "queen",
            Self::King => "king",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pawn" => Some(Self::Pawn),
            "knight" => Some(Self::Knight),
            "bishop" => Some(Self::Bishop),
            "rook" => Some(Self::Rook),
            "queen" => Some(Self::Queen),
            "king" => Some(Self::King),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PieceId(String);

impl PieceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        let Some((kind, stars)) = value.split_once('_') else {
            return Err("piece id must look like pawn_3".to_owned());
        };
        if value.matches('_').count() != 1 {
            return Err("piece id must contain exactly one underscore".to_owned());
        }
        let Some(piece) = PieceType::parse(kind) else {
            return Err("piece type must be pawn, knight, bishop, rook, queen, or king".to_owned());
        };
        if stars.is_empty() || !stars.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("piece stars must be an integer from 1 to 25".to_owned());
        }
        if stars.starts_with('0') {
            return Err("piece stars cannot be zero or have leading zeros".to_owned());
        }
        let parsed: u8 = stars
            .parse()
            .map_err(|_| "piece stars must be an integer from 1 to 25".to_owned())?;
        if parsed == 0 || parsed > MAX_PIECE_STARS {
            return Err(format!(
                "piece stars must be from 1 through {MAX_PIECE_STARS}"
            ));
        }
        Ok(Self(format!("{}_{parsed}", piece.as_str())))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Attachment {
    Currency {
        id: CurrencyId,
        #[serde(deserialize_with = "deserialize_positive_u32")]
        amount: u32,
    },
    Piece {
        #[serde(deserialize_with = "deserialize_piece_id")]
        id: PieceId,
        #[serde(deserialize_with = "deserialize_positive_u32")]
        amount: u32,
    },
}

impl Attachment {
    pub fn identity(&self) -> ( &'static str, String) {
        match self {
            Self::Currency { id, .. } => ("currency", id.as_str().to_owned()),
            Self::Piece { id, .. } => ("piece", id.as_str().to_owned()),
        }
    }
}

fn deserialize_piece_id<'de, D>(deserializer: D) -> Result<PieceId, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    PieceId::parse(&value).map_err(de::Error::custom)
}

fn deserialize_positive_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    struct PositiveVisitor;

    impl<'de> Visitor<'de> for PositiveVisitor {
        type Value = u32;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a positive integer")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value == 0 || value > u32::MAX as u64 {
                return Err(E::custom(
                    "amount must be a positive integer no greater than 4294967295",
                ));
            }
            Ok(value as u32)
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value <= 0 {
                return Err(E::custom("amount must be a positive integer"));
            }
            self.visit_u64(value as u64)
        }

        fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom(
                "amount must be an integer, not a floating-point number",
            ))
        }

        fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("amount must be a JSON number, not a string"))
        }
    }

    deserializer.deserialize_any(PositiveVisitor)
}
