//! The two places a miner changes something: choosing a password the first time
//! they enter their address, and using it afterwards to change their settings.
//!
//! Anyone can open the pool's page and type an address, so nothing here may take
//! the word of whoever is asking. A password can only be set once per address
//! (and only for an address the pool has seen), and every later change needs it.

use crate::ApiState;
use crate::handlers::{ApiError, Money, money};
use crate::passwords::{check_new_password, hash_password, verify_password};
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use pool_chain::Chain;
use pool_db::DbError;
use pool_domain::{Amount, MinerAddress};
use pool_payout::HIGHEST_PAYOUT_LEVEL_COINS;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;

/// Wrong passwords in a row before an address is locked.
pub const MAX_WRONG_PASSWORDS: u32 = 5;
/// How long a locked address stays locked.
pub const LOCK_MS: u64 = 15 * 60 * 1000;

#[derive(Deserialize)]
pub(crate) struct NewPassword {
    password: String,
}

#[derive(Deserialize)]
pub(crate) struct Change {
    password: String,
    payout_level: Option<String>,
    new_password: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Done {
    ok: bool,
    message: String,
    payout_level: Option<Money>,
}

/// A request that is not the JSON we asked for, as a sentence.
fn unreadable(rejection: JsonRejection) -> ApiError {
    let message = match rejection.status() {
        StatusCode::PAYLOAD_TOO_LARGE => "That request is too big.",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "That request must be sent as JSON.",
        _ => "That request was not understood.",
    };
    ApiError::new(rejection.status(), message)
}

/// Passwords are hashed slowly on purpose, and a flood of attempts could keep
/// every processor busy. Only a few are checked at a time; the rest are asked to wait.
fn work_permit(state: &ApiState) -> Result<OwnedSemaphorePermit, ApiError> {
    Arc::clone(&state.password_work).try_acquire_owned().map_err(|_| {
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "The pool is busy checking passwords. Please try again in a moment.")
    })
}

async fn hash_off_thread(password: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || hash_password(&password)).await.map_err(|_| internal())?.map_err(|_| internal())
}

async fn verify_off_thread(password: String, stored: String) -> Result<bool, ApiError> {
    tokio::task::spawn_blocking(move || verify_password(&password, &stored)).await.map_err(|_| internal())
}

fn internal() -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the pool's side. Please try again in a moment.")
}

fn db_problem(error: DbError) -> ApiError {
    ApiError::from(error)
}

/// A level a miner asked for, or a sentence saying why not.
fn check_level<C: Chain>(text: &str, lowest: Amount) -> Result<Amount, ApiError> {
    let bad = |message: String| ApiError::new(StatusCode::BAD_REQUEST, message);
    let level = Amount::parse_coins(text).ok_or_else(|| bad("That is not a number of coins. Write it like 25 or 12.5.".to_owned()))?;
    if level < lowest {
        return Err(bad(format!(
            "The lowest payout level is {}. Anything lower does not work, because small payments cost the network more to send than they are worth.",
            lowest.display::<C>()
        )));
    }
    let highest = Amount::from_coins(HIGHEST_PAYOUT_LEVEL_COINS).unwrap_or(Amount::new(u64::MAX));
    if level > highest {
        return Err(bad(format!("The highest payout level you can choose is {}.", highest.display::<C>())));
    }
    Ok(level)
}

fn parse_address<C: Chain>(state: &ApiState, text: &str) -> Result<MinerAddress, ApiError> {
    MinerAddress::parse::<C>(&state.network, text).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))
}

/// Choose a password for an address, the first time.
pub(crate) async fn set_password<C: Chain>(
    State(state): State<Arc<ApiState>>,
    Path(address): Path<String>,
    body: Result<Json<NewPassword>, JsonRejection>,
) -> Result<Json<Done>, ApiError> {
    let Json(body) = body.map_err(unreadable)?;
    let address = parse_address::<C>(&state, &address)?;
    check_new_password(&body.password).map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;

    if state.db.miner_account(&address).await.map_err(db_problem)?.is_some() {
        return Err(ApiError::new(StatusCode::CONFLICT, "This address already has a password. Enter it to change your settings."));
    }
    // Only an address the pool has seen: nobody can reserve addresses in advance.
    if !state.db.address_is_known(&address).await.map_err(db_problem)? {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "The pool has not seen any work or earnings from this address yet, so a password cannot be set for it. Start mining with it, wait a minute, then try again.",
        ));
    }

    let _permit = work_permit(&state)?;
    let hash = hash_off_thread(body.password).await?;
    match state.db.create_miner_account(&address, &hash, None, (state.clock)()).await {
        Ok(()) => Ok(Json(Done {
            ok: true,
            message: "Your password is set. You will need it every time you change a setting, so keep it safe. The pool cannot show it to you again.".to_owned(),
            payout_level: None,
        })),
        Err(DbError::AccountExists(_)) => Err(ApiError::new(StatusCode::CONFLICT, "This address already has a password. Enter it to change your settings.")),
        Err(other) => Err(db_problem(other)),
    }
}

/// Change a miner's settings, with their password.
pub(crate) async fn change_settings<C: Chain>(
    State(state): State<Arc<ApiState>>,
    Path(address): Path<String>,
    body: Result<Json<Change>, JsonRejection>,
) -> Result<Json<Done>, ApiError> {
    let Json(body) = body.map_err(unreadable)?;
    let address = parse_address::<C>(&state, &address)?;
    if body.payout_level.is_none() && body.new_password.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "There is nothing to change. Enter a new payout level, a new password, or both.",
        ));
    }
    // Check what was asked before spending any effort on the password.
    let level = match &body.payout_level {
        Some(text) => Some(check_level::<C>(text, state.min_payout)?),
        None => None,
    };
    if let Some(new_password) = &body.new_password {
        check_new_password(new_password).map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, m))?;
    }

    let account = state
        .db
        .miner_account(&address)
        .await
        .map_err(db_problem)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "No password has been set for this address yet. Set one first."))?;

    let now = (state.clock)();
    if account.locked_until_ms > now {
        let minutes = (account.locked_until_ms - now).div_ceil(60_000);
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "There have been too many wrong passwords for this address. Try again in {minutes} minute{}.",
                if minutes == 1 { "" } else { "s" }
            ),
        ));
    }

    let permit = work_permit(&state)?;
    let right = verify_off_thread(body.password, account.password_hash.clone()).await?;
    if !right {
        let locked_until = state.db.note_wrong_password(&address, now, MAX_WRONG_PASSWORDS, LOCK_MS).await.map_err(db_problem)?;
        drop(permit);
        return Err(if locked_until > 0 {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "That password is not right, and there have now been too many wrong ones. This address is locked for {} minutes.",
                    LOCK_MS / 60_000
                ),
            )
        } else {
            ApiError::new(StatusCode::FORBIDDEN, "That password is not right.")
        });
    }
    state.db.note_right_password(&address).await.map_err(db_problem)?;

    let new_hash = match body.new_password {
        Some(new_password) => Some(hash_off_thread(new_password).await?),
        None => None,
    };
    drop(permit);
    state.db.update_miner_account(&address, new_hash.as_deref(), level, now).await.map_err(db_problem)?;

    let mut said = Vec::new();
    if let Some(level) = level {
        said.push(format!("Your payout level is now {}. You will be paid once you are owed at least that.", level.display::<C>()));
    }
    if new_hash.is_some() {
        said.push("Your password has been changed.".to_owned());
    }
    Ok(Json(Done { ok: true, message: said.join(" "), payout_level: level.map(money::<C>) }))
}
