#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, Env,
};

const SESSION_TTL_BUMP: u32 = 518_400;

/// Stateful Game Hub substitute used to exercise game lifecycle integration.
///
/// This contract records the exact terms accepted at `start_game` and enforces
/// owner authorization and exactly-once terminal settlement. It deliberately
/// does not model balances or claim to reproduce production Hub economics.
#[contract]
pub struct MockGameHub;

/// Existing start event. Its field names and order are part of the mock ABI.
#[contractevent]
pub struct GameStarted {
    pub session_id: u32,
    pub game_id: Address,
    pub player1: Address,
    pub player2: Address,
    pub player1_points: i128,
    pub player2_points: i128,
}

/// Existing winner-settlement event. Its shape remains unchanged.
#[contractevent]
pub struct GameEnded {
    pub session_id: u32,
    pub player1_won: bool,
}

/// Neutral settlement event. No winner is assigned; each participant receives
/// the exact stake supplied for that participant at `start_game`.
#[contractevent]
pub struct GameCancelled {
    pub session_id: u32,
    pub player1: Address,
    pub player2: Address,
    pub player1_refund: i128,
    pub player2_refund: i128,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum MockGameHubError {
    SessionAlreadyExists = 1,
    SessionNotFound = 2,
    SessionAlreadyTerminal = 3,
    SelfPlay = 4,
    InvalidStake = 5,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStatus {
    Active,
    Ended,
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Session {
    pub game_id: Address,
    pub player1: Address,
    pub player2: Address,
    pub player1_points: i128,
    pub player2_points: i128,
    pub status: SessionStatus,
    pub player1_won: Option<bool>,
    pub player1_refund: i128,
    pub player2_refund: i128,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Session(u32),
}

fn get_session(env: &Env, session_id: u32) -> Result<Session, MockGameHubError> {
    env.storage()
        .persistent()
        .get(&DataKey::Session(session_id))
        .ok_or(MockGameHubError::SessionNotFound)
}

fn put_session(env: &Env, session_id: u32, session: &Session) {
    let key = DataKey::Session(session_id);
    env.storage().persistent().set(&key, session);
    env.storage()
        .persistent()
        .extend_ttl(&key, SESSION_TTL_BUMP, SESSION_TTL_BUMP);
    env.storage()
        .instance()
        .extend_ttl(SESSION_TTL_BUMP, SESSION_TTL_BUMP);
}

#[contractimpl]
impl MockGameHub {
    /// Records and locks the exact terms of a session in the mock lifecycle.
    pub fn start_game(
        env: Env,
        game_id: Address,
        session_id: u32,
        player1: Address,
        player2: Address,
        player1_points: i128,
        player2_points: i128,
    ) -> Result<(), MockGameHubError> {
        game_id.require_auth();

        if player1 == player2 {
            return Err(MockGameHubError::SelfPlay);
        }
        if player1_points <= 0 || player2_points <= 0 {
            return Err(MockGameHubError::InvalidStake);
        }

        let key = DataKey::Session(session_id);
        if env.storage().persistent().has(&key) {
            return Err(MockGameHubError::SessionAlreadyExists);
        }

        put_session(
            &env,
            session_id,
            &Session {
                game_id: game_id.clone(),
                player1: player1.clone(),
                player2: player2.clone(),
                player1_points,
                player2_points,
                status: SessionStatus::Active,
                player1_won: None,
                player1_refund: 0,
                player2_refund: 0,
            },
        );

        GameStarted {
            session_id,
            game_id,
            player1,
            player2,
            player1_points,
            player2_points,
        }
        .publish(&env);

        Ok(())
    }

    /// Settles an active session with exactly one winner.
    pub fn end_game(env: Env, session_id: u32, player1_won: bool) -> Result<(), MockGameHubError> {
        let mut session = get_session(&env, session_id)?;
        session.game_id.require_auth();
        if session.status != SessionStatus::Active {
            return Err(MockGameHubError::SessionAlreadyTerminal);
        }

        session.status = SessionStatus::Ended;
        session.player1_won = Some(player1_won);
        put_session(&env, session_id, &session);

        GameEnded {
            session_id,
            player1_won,
        }
        .publish(&env);

        Ok(())
    }

    /// Cancels an active session neutrally and records exact asymmetric refunds.
    pub fn cancel_game(env: Env, session_id: u32) -> Result<(), MockGameHubError> {
        let mut session = get_session(&env, session_id)?;
        session.game_id.require_auth();
        if session.status != SessionStatus::Active {
            return Err(MockGameHubError::SessionAlreadyTerminal);
        }

        session.status = SessionStatus::Cancelled;
        session.player1_refund = session.player1_points;
        session.player2_refund = session.player2_points;
        put_session(&env, session_id, &session);

        GameCancelled {
            session_id,
            player1: session.player1,
            player2: session.player2,
            player1_refund: session.player1_refund,
            player2_refund: session.player2_refund,
        }
        .publish(&env);

        Ok(())
    }

    /// Returns the stored lifecycle record for focused integration assertions.
    pub fn session(env: Env, session_id: u32) -> Result<Session, MockGameHubError> {
        get_session(&env, session_id)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events as _},
        Event as _,
    };

    /// Calls the Hub as a real contract owner. No global auth mocking is used,
    /// so `game_id.require_auth()` is exercised by the host authorization tree.
    #[contract]
    struct GameOwner;

    #[contractimpl]
    impl GameOwner {
        pub fn start(
            env: Env,
            hub: Address,
            session_id: u32,
            player1: Address,
            player2: Address,
            player1_points: i128,
            player2_points: i128,
        ) -> Result<(), MockGameHubError> {
            MockGameHubClient::new(&env, &hub).start_game(
                &env.current_contract_address(),
                &session_id,
                &player1,
                &player2,
                &player1_points,
                &player2_points,
            );
            Ok(())
        }

        pub fn end(
            env: Env,
            hub: Address,
            session_id: u32,
            player1_won: bool,
        ) -> Result<(), MockGameHubError> {
            MockGameHubClient::new(&env, &hub).end_game(&session_id, &player1_won);
            Ok(())
        }

        pub fn cancel(env: Env, hub: Address, session_id: u32) -> Result<(), MockGameHubError> {
            MockGameHubClient::new(&env, &hub).cancel_game(&session_id);
            Ok(())
        }
    }

    fn setup() -> (Env, Address, GameOwnerClient<'static>, Address, Address) {
        let env = Env::default();
        let hub = env.register(MockGameHub, ());
        let owner = env.register(GameOwner, ());
        let owner_client = GameOwnerClient::new(&env, &owner);
        let player1 = Address::generate(&env);
        let player2 = Address::generate(&env);
        (env, hub, owner_client, player1, player2)
    }

    fn assert_hub_error<T, E>(
        result: &Result<Result<T, E>, Result<MockGameHubError, soroban_sdk::InvokeError>>,
        expected: MockGameHubError,
    ) {
        assert!(matches!(result, Err(Ok(actual)) if *actual == expected));
    }

    #[test]
    fn owner_auth_and_exact_asymmetric_refunds() {
        let (env, hub, owner, player1, player2) = setup();
        owner.start(&hub, &7, &player1, &player2, &11, &29);

        let active = MockGameHubClient::new(&env, &hub).session(&7);
        assert_eq!(active.status, SessionStatus::Active);
        assert_eq!(active.player1_points, 11);
        assert_eq!(active.player2_points, 29);

        owner.cancel(&hub, &7);
        assert_eq!(
            env.events().all().filter_by_contract(&hub),
            [GameCancelled {
                session_id: 7,
                player1,
                player2,
                player1_refund: 11,
                player2_refund: 29,
            }
            .to_xdr(&env, &hub)],
        );

        let cancelled = MockGameHubClient::new(&env, &hub).session(&7);
        assert_eq!(cancelled.status, SessionStatus::Cancelled);
        assert_eq!(cancelled.player1_won, None);
        assert_eq!(cancelled.player1_refund, 11);
        assert_eq!(cancelled.player2_refund, 29);
    }

    #[test]
    fn duplicate_start_is_typed_error() {
        let (_env, hub, owner, player1, player2) = setup();
        owner.start(&hub, &8, &player1, &player2, &3, &5);
        let duplicate = owner.try_start(&hub, &8, &player1, &player2, &3, &5);
        assert_hub_error(&duplicate, MockGameHubError::SessionAlreadyExists);
    }

    #[test]
    fn cancellation_and_winner_settlement_are_mutually_exclusive() {
        let (env, hub, owner, player1, player2) = setup();

        owner.start(&hub, &9, &player1, &player2, &7, &13);
        owner.cancel(&hub, &9);
        let duplicate_cancel = owner.try_cancel(&hub, &9);
        assert_hub_error(&duplicate_cancel, MockGameHubError::SessionAlreadyTerminal);
        assert!(env
            .events()
            .all()
            .filter_by_contract(&hub)
            .events()
            .is_empty());
        let end_after_cancel = owner.try_end(&hub, &9, &true);
        assert_hub_error(&end_after_cancel, MockGameHubError::SessionAlreadyTerminal);

        owner.start(&hub, &10, &player1, &player2, &17, &19);
        owner.end(&hub, &10, &false);
        let duplicate_end = owner.try_end(&hub, &10, &true);
        assert_hub_error(&duplicate_end, MockGameHubError::SessionAlreadyTerminal);
        let cancel_after_end = owner.try_cancel(&hub, &10);
        assert_hub_error(&cancel_after_end, MockGameHubError::SessionAlreadyTerminal);

        let ended = MockGameHubClient::new(&env, &hub).session(&10);
        assert_eq!(ended.status, SessionStatus::Ended);
        assert_eq!(ended.player1_won, Some(false));
        assert_eq!(ended.player1_refund, 0);
        assert_eq!(ended.player2_refund, 0);
    }

    #[test]
    fn invalid_terms_are_typed_errors() {
        let (_env, hub, owner, player1, player2) = setup();

        let missing = owner.try_end(&hub, &999, &true);
        assert_hub_error(&missing, MockGameHubError::SessionNotFound);

        let self_play = owner.try_start(&hub, &11, &player1, &player1, &1, &1);
        assert_hub_error(&self_play, MockGameHubError::SelfPlay);

        let zero_stake = owner.try_start(&hub, &12, &player1, &player2, &1, &0);
        assert_hub_error(&zero_stake, MockGameHubError::InvalidStake);
    }

    #[test]
    fn a_different_contract_cannot_settle_or_cancel_the_session() {
        let (env, hub, owner, player1, player2) = setup();
        owner.start(&hub, &13, &player1, &player2, &23, &31);

        let other_owner_id = env.register(GameOwner, ());
        let other_owner = GameOwnerClient::new(&env, &other_owner_id);
        let unauthorized = other_owner.try_end(&hub, &13, &true);
        assert!(matches!(unauthorized, Err(Err(_))));
        let unauthorized = other_owner.try_cancel(&hub, &13);
        assert!(matches!(unauthorized, Err(Err(_))));

        let session = MockGameHubClient::new(&env, &hub).session(&13);
        assert_eq!(session.status, SessionStatus::Active);
    }
}
