#![no_std]

//! A two-player dice game using session-bound commit/reveal randomness.
//!
//! Both commitments and the exact game terms are authorized before the Game
//! Hub locks either stake. Reveals are accepted through the inclusive ledger
//! deadline. The dice are derived from the two secrets in player-role order,
//! so neither public session inputs nor reveal order influence the result.

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, xdr::ToXdr, Address,
    Bytes, BytesN, Env,
};

#[contractclient(name = "GameHubClient")]
pub trait GameHub {
    fn start_game(
        env: Env,
        game_id: Address,
        session_id: u32,
        player1: Address,
        player2: Address,
        player1_points: i128,
        player2_points: i128,
    );

    fn end_game(env: Env, session_id: u32, player1_won: bool);

    fn cancel_game(env: Env, session_id: u32);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    // Codes 1-5 are the legacy Dice Duel ABI and must not be renumbered.
    GameNotFound = 1,
    NotPlayer = 2,
    AlreadyRolled = 3,
    BothPlayersNotRolled = 4,
    GameAlreadyEnded = 5,
    SamePlayer = 6,
    GameAlreadyExists = 7,
    InvalidDeadline = 8,
    RevealDeadlinePassed = 9,
    RevealDeadlineNotReached = 10,
    WrongSecret = 11,
    FinalizationInProgress = 12,
}

pub const PLAYER1_ROLE: u32 = 1;
pub const PLAYER2_ROLE: u32 = 2;
pub const COMMITMENT_DOMAIN: &[u8] = b"stellar-game-studio:commitment:v1";
pub const OUTCOME_DOMAIN: &[u8] = b"stellar-game-studio:dice-outcome:v1";
pub const GAME_TAG: &[u8] = b"dice-duel";

/// Persistent game entries remain live for 30 days at approximately five
/// seconds per ledger. If left untouched longer, they can be restored from
/// archival before settlement instead of being deleted irreversibly.
pub const GAME_TTL_LEDGERS: u32 = 518_400;
pub const TIMEOUT_RESOLUTION_GRACE_LEDGERS: u32 = 17_280;
pub const MAX_REVEAL_WINDOW_LEDGERS: u32 = GAME_TTL_LEDGERS - TIMEOUT_RESOLUTION_GRACE_LEDGERS;

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Revealing,
    Ready,
    Finalizing,
    Settled,
    Forfeited,
    Cancelled,
}

/// A named Soroban value is used so `to_xdr` produces one canonical SCV_MAP.
/// Every field is load-bearing replay context.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitmentPreimage {
    pub domain: Bytes,
    pub contract: Address,
    pub game: Bytes,
    pub session_id: u32,
    pub role: u32,
    pub revealing_player: Address,
    pub player1: Address,
    pub player2: Address,
    pub player1_points: i128,
    pub player2_points: i128,
    pub secret: BytesN<32>,
}

/// Outcome entropy intentionally contains no session or other public input.
/// Secrets always appear in player1/player2 order, never reveal order.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutcomePreimage {
    pub domain: Bytes,
    pub player1_secret: BytesN<32>,
    pub player2_secret: BytesN<32>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    pub player1: Address,
    pub player2: Address,
    pub player1_points: i128,
    pub player2_points: i128,
    pub game_hub: Address,
    pub player1_commitment: BytesN<32>,
    pub player2_commitment: BytesN<32>,
    pub reveal_deadline: u32,
    pub phase: Phase,
    pub player1_rolled: bool,
    pub player2_rolled: bool,
    /// Only the first valid reveal must be retained. It is cleared as soon as
    /// the second reveal deterministically prepares the dice.
    pub first_secret: Option<BytesN<32>>,
    pub player1_die1: Option<u32>,
    pub player1_die2: Option<u32>,
    pub player2_die1: Option<u32>,
    pub player2_die2: Option<u32>,
    pub winner: Option<Address>,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Game(u32),
    GameHubAddress,
    Admin,
}

#[allow(clippy::too_many_arguments)]
fn commitment_digest(
    env: &Env,
    session_id: u32,
    role: u32,
    revealing_player: &Address,
    player1: &Address,
    player2: &Address,
    player1_points: i128,
    player2_points: i128,
    secret: &BytesN<32>,
) -> BytesN<32> {
    let preimage = CommitmentPreimage {
        domain: Bytes::from_slice(env, COMMITMENT_DOMAIN),
        contract: env.current_contract_address(),
        game: Bytes::from_slice(env, GAME_TAG),
        session_id,
        role,
        revealing_player: revealing_player.clone(),
        player1: player1.clone(),
        player2: player2.clone(),
        player1_points,
        player2_points,
        secret: secret.clone(),
    };
    env.crypto().sha256(&preimage.to_xdr(env)).to_bytes()
}

fn prepare_dice(
    env: &Env,
    player1_secret: &BytesN<32>,
    player2_secret: &BytesN<32>,
) -> (u32, u32, u32, u32) {
    let preimage = OutcomePreimage {
        domain: Bytes::from_slice(env, OUTCOME_DOMAIN),
        player1_secret: player1_secret.clone(),
        player2_secret: player2_secret.clone(),
    };
    let seed = env.crypto().sha256(&preimage.to_xdr(env));
    env.prng().seed(seed.into());
    (
        env.prng().gen_range::<u64>(1..=6) as u32,
        env.prng().gen_range::<u64>(1..=6) as u32,
        env.prng().gen_range::<u64>(1..=6) as u32,
        env.prng().gen_range::<u64>(1..=6) as u32,
    )
}

fn put_game(env: &Env, session_id: u32, game: &Game) {
    let key = DataKey::Game(session_id);
    env.storage().persistent().set(&key, game);
    // Every game-state write refreshes retention. If a Hub invocation fails,
    // Soroban rolls this write and the nested invocation back atomically.
    env.storage()
        .persistent()
        .extend_ttl(&key, GAME_TTL_LEDGERS, GAME_TTL_LEDGERS);
    // Refresh the contract instance/code together with game state. Otherwise
    // a game opened late in the previous instance lifetime could outlive the
    // contract that must settle it.
    env.storage()
        .instance()
        .extend_ttl(GAME_TTL_LEDGERS, GAME_TTL_LEDGERS);
}

fn is_terminal(phase: Phase) -> bool {
    matches!(phase, Phase::Settled | Phase::Forfeited | Phase::Cancelled)
}

fn end_hub_game(env: &Env, hub_address: &Address, session_id: u32, player1_won: bool) {
    GameHubClient::new(env, hub_address).end_game(&session_id, &player1_won);
}

/// Kept isolated because production Hub cancellation compatibility remains a
/// deployment gate independent of the Dice Duel lifecycle implementation.
fn cancel_hub_game(env: &Env, hub_address: &Address, session_id: u32) {
    GameHubClient::new(env, hub_address).cancel_game(&session_id);
}

#[contract]
pub struct DiceDuelContract;

#[contractimpl]
impl DiceDuelContract {
    pub fn __constructor(env: Env, admin: Address, game_hub: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::GameHubAddress, &game_hub);
        env.storage()
            .instance()
            .extend_ttl(GAME_TTL_LEDGERS, GAME_TTL_LEDGERS);
    }

    /// Return the commitment expected by `roll` for this contract instance.
    #[allow(clippy::too_many_arguments)]
    pub fn commitment(
        env: Env,
        session_id: u32,
        role: u32,
        revealing_player: Address,
        player1: Address,
        player2: Address,
        player1_points: i128,
        player2_points: i128,
        secret: BytesN<32>,
    ) -> BytesN<32> {
        commitment_digest(
            &env,
            session_id,
            role,
            &revealing_player,
            &player1,
            &player2,
            player1_points,
            player2_points,
            &secret,
        )
    }

    /// Both signers authorize the complete game intent before the Hub lock.
    #[allow(clippy::too_many_arguments)]
    pub fn start_game(
        env: Env,
        session_id: u32,
        player1: Address,
        player2: Address,
        player1_points: i128,
        player2_points: i128,
        player1_commitment: BytesN<32>,
        player2_commitment: BytesN<32>,
        reveal_deadline: u32,
    ) -> Result<(), Error> {
        if player1 == player2 {
            return Err(Error::SamePlayer);
        }

        let current_ledger = env.ledger().sequence();
        let reveal_window = reveal_deadline
            .checked_sub(current_ledger)
            .ok_or(Error::InvalidDeadline)?;
        if reveal_window == 0 || reveal_window > MAX_REVEAL_WINDOW_LEDGERS {
            return Err(Error::InvalidDeadline);
        }

        let key = DataKey::Game(session_id);
        if env.storage().persistent().has(&key) {
            return Err(Error::GameAlreadyExists);
        }

        // This binds each signer to every argument of the current invocation.
        player1.require_auth();
        player2.require_auth();

        let game_hub: Address = env
            .storage()
            .instance()
            .get(&DataKey::GameHubAddress)
            .ok_or(Error::GameNotFound)?;

        let game = Game {
            player1: player1.clone(),
            player2: player2.clone(),
            player1_points,
            player2_points,
            game_hub: game_hub.clone(),
            player1_commitment,
            player2_commitment,
            reveal_deadline,
            phase: Phase::Revealing,
            player1_rolled: false,
            player2_rolled: false,
            first_secret: None,
            player1_die1: None,
            player1_die2: None,
            player2_die1: None,
            player2_die2: None,
            winner: None,
        };

        // The entire invocation is atomic: a rejected Hub call rolls back this
        // pre-lock state write.
        put_game(&env, session_id, &game);
        GameHubClient::new(&env, &game_hub).start_game(
            &env.current_contract_address(),
            &session_id,
            &player1,
            &player2,
            &player1_points,
            &player2_points,
        );
        Ok(())
    }

    /// Verify one reveal. The second valid reveal prepares but does not settle.
    pub fn roll(
        env: Env,
        session_id: u32,
        player: Address,
        secret: BytesN<32>,
    ) -> Result<(), Error> {
        let mut game: Game = env
            .storage()
            .persistent()
            .get(&DataKey::Game(session_id))
            .ok_or(Error::GameNotFound)?;

        if game.phase == Phase::Finalizing {
            return Err(Error::FinalizationInProgress);
        }
        if is_terminal(game.phase) {
            return Err(Error::GameAlreadyEnded);
        }

        let (role, already_revealed, expected_commitment) = if player == game.player1 {
            (
                PLAYER1_ROLE,
                game.player1_rolled,
                game.player1_commitment.clone(),
            )
        } else if player == game.player2 {
            (
                PLAYER2_ROLE,
                game.player2_rolled,
                game.player2_commitment.clone(),
            )
        } else {
            return Err(Error::NotPlayer);
        };

        if already_revealed {
            return Err(Error::AlreadyRolled);
        }
        if game.phase != Phase::Revealing {
            return Err(Error::GameAlreadyEnded);
        }
        if env.ledger().sequence() > game.reveal_deadline {
            return Err(Error::RevealDeadlinePassed);
        }

        player.require_auth();
        let actual_commitment = commitment_digest(
            &env,
            session_id,
            role,
            &player,
            &game.player1,
            &game.player2,
            game.player1_points,
            game.player2_points,
            &secret,
        );
        if actual_commitment != expected_commitment {
            return Err(Error::WrongSecret);
        }

        let was_player1_revealed = game.player1_rolled;
        let was_player2_revealed = game.player2_rolled;
        if role == PLAYER1_ROLE {
            game.player1_rolled = true;
        } else {
            game.player2_rolled = true;
        }

        if !was_player1_revealed && !was_player2_revealed {
            game.first_secret = Some(secret);
            put_game(&env, session_id, &game);
            return Ok(());
        }

        let first_secret = game.first_secret.clone().ok_or(Error::GameNotFound)?;
        let (player1_secret, player2_secret) = if was_player1_revealed {
            (first_secret, secret)
        } else {
            (secret, first_secret)
        };
        let (player1_die1, player1_die2, player2_die1, player2_die2) =
            prepare_dice(&env, &player1_secret, &player2_secret);

        game.first_secret = None;
        game.player1_die1 = Some(player1_die1);
        game.player1_die2 = Some(player1_die2);
        game.player2_die1 = Some(player2_die1);
        game.player2_die2 = Some(player2_die2);
        game.winner = Some(
            if player1_die1 + player1_die2 >= player2_die1 + player2_die2 {
                game.player1.clone()
            } else {
                game.player2.clone()
            },
        );
        game.phase = Phase::Ready;
        put_game(&env, session_id, &game);
        Ok(())
    }

    /// Permissionlessly settle a Ready game through its snapshotted Hub.
    pub fn reveal_winner(env: Env, session_id: u32) -> Result<Address, Error> {
        let mut game: Game = env
            .storage()
            .persistent()
            .get(&DataKey::Game(session_id))
            .ok_or(Error::GameNotFound)?;

        if game.phase == Phase::Finalizing {
            return Err(Error::FinalizationInProgress);
        }
        if is_terminal(game.phase) {
            return Err(Error::GameAlreadyEnded);
        }
        if game.phase != Phase::Ready {
            return Err(Error::BothPlayersNotRolled);
        }

        let winner = game.winner.clone().ok_or(Error::BothPlayersNotRolled)?;
        let player1_won = winner == game.player1;

        game.phase = Phase::Finalizing;
        put_game(&env, session_id, &game);
        end_hub_game(&env, &game.game_hub, session_id, player1_won);
        game.phase = Phase::Settled;
        put_game(&env, session_id, &game);
        Ok(winner)
    }

    /// Strictly after the inclusive deadline, one reveal wins by forfeit and
    /// zero reveals cause neutral cancellation.
    pub fn resolve_timeout(env: Env, session_id: u32) -> Result<Option<Address>, Error> {
        let mut game: Game = env
            .storage()
            .persistent()
            .get(&DataKey::Game(session_id))
            .ok_or(Error::GameNotFound)?;

        if game.phase == Phase::Finalizing {
            return Err(Error::FinalizationInProgress);
        }
        if is_terminal(game.phase) {
            return Err(Error::GameAlreadyEnded);
        }
        if game.phase != Phase::Revealing {
            return Err(Error::GameAlreadyEnded);
        }
        if env.ledger().sequence() <= game.reveal_deadline {
            return Err(Error::RevealDeadlineNotReached);
        }

        let winner = match (game.player1_rolled, game.player2_rolled) {
            (true, false) => Some(game.player1.clone()),
            (false, true) => Some(game.player2.clone()),
            (false, false) => None,
            // Both valid reveals atomically transition to Ready.
            (true, true) => return Err(Error::GameAlreadyEnded),
        };

        game.first_secret = None;
        game.winner = winner.clone();
        game.phase = Phase::Finalizing;
        put_game(&env, session_id, &game);

        if let Some(ref revealed_winner) = winner {
            let player1_won = *revealed_winner == game.player1;
            end_hub_game(&env, &game.game_hub, session_id, player1_won);
            game.phase = Phase::Forfeited;
        } else {
            cancel_hub_game(&env, &game.game_hub, session_id);
            game.phase = Phase::Cancelled;
        }

        put_game(&env, session_id, &game);
        Ok(winner)
    }

    pub fn get_game(env: Env, session_id: u32) -> Result<Game, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Game(session_id))
            .ok_or(Error::GameNotFound)
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Admin not set")
    }

    pub fn set_admin(env: Env, new_admin: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Admin not set");
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.storage()
            .instance()
            .extend_ttl(GAME_TTL_LEDGERS, GAME_TTL_LEDGERS);
    }

    pub fn get_hub(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::GameHubAddress)
            .expect("GameHub address not set")
    }

    pub fn set_hub(env: Env, new_hub: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Admin not set");
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::GameHubAddress, &new_hub);
        env.storage()
            .instance()
            .extend_ttl(GAME_TTL_LEDGERS, GAME_TTL_LEDGERS);
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Admin not set");
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }
}

#[cfg(test)]
mod test;
