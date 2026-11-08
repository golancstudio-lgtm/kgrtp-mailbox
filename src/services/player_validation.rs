use crate::models::AppError;
use crate::ugs::{map_transport, PlayerAuth, PlayerRecord, UgsError};
use crate::validation::validate_player_id;

#[derive(Clone)]
pub struct PlayerValidation {
    players: PlayerAuth,
}

impl PlayerValidation {
    pub fn new(players: PlayerAuth) -> Self {
        Self { players }
    }

    pub async fn require_existing(&self, player_id: &str) -> Result<PlayerRecord, AppError> {
        validate_player_id(player_id)?;
        match self.players.get_player(player_id).await {
            Ok(player) => Ok(player),
            Err(UgsError::NotFound) => Err(AppError::player_not_found()),
            Err(UgsError::BadRequest) => Err(AppError::invalid_player_id()),
            Err(error) => Err(map_transport(error)),
        }
    }
}
