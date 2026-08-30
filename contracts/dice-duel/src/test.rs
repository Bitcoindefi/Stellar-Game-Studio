#![cfg(test)]

extern crate std;

use crate::{
    DataKey, DiceDuelContract, DiceDuelContractClient, Error, Phase, GAME_TTL_LEDGERS,
    MAX_REVEAL_WINDOW_LEDGERS, PLAYER1_ROLE, PLAYER2_ROLE,
};
use mock_game_hub::{MockGameHub, MockGameHubClient, MockGameHubError, SessionStatus};
use soroban_sdk::testutils::{
    Address as _, AuthorizedFunction, AuthorizedInvocation, Deployer as _, Ledger as _,
};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, IntoVal, Symbol,
};

const LEDGER: u32 = 100;
const DEADLINE: u32 = 200;
const PLAYER1_POINTS: i128 = 17;
const PLAYER2_POINTS: i128 = 29;

struct Fixture {
    env: Env,
    client: DiceDuelContractClient<'static>,
    hub: MockGameHubClient<'static>,
    hub_address: Address,
    admin: Address,
    player1: Address,
    player2: Address,
    secret1: BytesN<32>,
    secret2: BytesN<32>,
}

fn ledger(env: &Env, sequence: u32) {
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 1_700_000_000,
        protocol_version: 27,
        sequence_number: sequence,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 100_000,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 1_000_000,
    });
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    ledger(&env, LEDGER);

    let hub_address = env.register(MockGameHub, ());
    let hub = MockGameHubClient::new(&env, &hub_address);
    let admin = Address::generate(&env);
    let contract = env.register(DiceDuelContract, (&admin, &hub_address));
    let client = DiceDuelContractClient::new(&env, &contract);

    Fixture {
        env: env.clone(),
        client,
        hub,
        hub_address,
        admin,
        player1: Address::generate(&env),
        player2: Address::generate(&env),
        secret1: BytesN::from_array(&env, &[0x11; 32]),
        secret2: BytesN::from_array(&env, &[0x22; 32]),
    }
}

impl Fixture {
    fn commitment(
        &self,
        session_id: u32,
        role: u32,
        revealing_player: &Address,
        player1_points: i128,
        player2_points: i128,
        secret: &BytesN<32>,
    ) -> BytesN<32> {
        self.client.commitment(
            &session_id,
            &role,
            revealing_player,
            &self.player1,
            &self.player2,
            &player1_points,
            &player2_points,
            secret,
        )
    }

    fn commitments(&self, session_id: u32) -> (BytesN<32>, BytesN<32>) {
        (
            self.commitment(
                session_id,
                PLAYER1_ROLE,
                &self.player1,
                PLAYER1_POINTS,
                PLAYER2_POINTS,
                &self.secret1,
            ),
            self.commitment(
                session_id,
                PLAYER2_ROLE,
                &self.player2,
                PLAYER1_POINTS,
                PLAYER2_POINTS,
                &self.secret2,
            ),
        )
    }

    fn start(&self, session_id: u32) {
        self.start_at(session_id, DEADLINE);
    }

    fn start_at(&self, session_id: u32, deadline: u32) {
        let (commitment1, commitment2) = self.commitments(session_id);
        self.start_with(session_id, commitment1, commitment2, deadline);
    }

    fn start_with(
        &self,
        session_id: u32,
        commitment1: BytesN<32>,
        commitment2: BytesN<32>,
        deadline: u32,
    ) {
        self.client.start_game(
            &session_id,
            &self.player1,
            &self.player2,
            &PLAYER1_POINTS,
            &PLAYER2_POINTS,
            &commitment1,
            &commitment2,
            &deadline,
        );
    }
}

fn assert_error<T, E>(
    result: &Result<Result<T, E>, Result<Error, soroban_sdk::InvokeError>>,
    expected: Error,
) {
    assert!(matches!(result, Err(Ok(actual)) if *actual == expected));
}

fn dice(game: &crate::Game) -> [u32; 4] {
    [
        game.player1_die1.unwrap(),
        game.player1_die2.unwrap(),
        game.player2_die1.unwrap(),
        game.player2_die2.unwrap(),
    ]
}

#[test]
fn complete_two_reveal_then_settle_flow() {
    let f = setup();
    f.start(1);

    let initial = f.client.get_game(&1);
    assert_eq!(initial.phase, Phase::Revealing);
    assert_eq!(initial.game_hub, f.hub_address);
    assert!(!initial.player1_rolled && !initial.player2_rolled);
    assert_eq!(initial.winner, None);

    f.client.roll(&1, &f.player1, &f.secret1);
    let first = f.client.get_game(&1);
    assert_eq!(first.phase, Phase::Revealing);
    assert!(first.player1_rolled && !first.player2_rolled);
    assert_eq!(first.first_secret, Some(f.secret1.clone()));
    assert_eq!(first.player1_die1, None);

    f.client.roll(&1, &f.player2, &f.secret2);
    let ready = f.client.get_game(&1);
    assert_eq!(ready.phase, Phase::Ready);
    assert!(ready.player1_rolled && ready.player2_rolled);
    assert_eq!(ready.first_secret, None);
    for die in dice(&ready) {
        assert!((1..=6).contains(&die));
    }
    let expected_winner = ready.winner.clone().unwrap();

    let winner = f.client.reveal_winner(&1);
    assert_eq!(winner, expected_winner);
    assert_eq!(f.client.get_game(&1).phase, Phase::Settled);
    let hub_session = f.hub.session(&1);
    assert_eq!(hub_session.status, SessionStatus::Ended);
    assert_eq!(hub_session.player1_won, Some(winner == f.player1));
    assert_eq!(hub_session.player1_points, PLAYER1_POINTS);
    assert_eq!(hub_session.player2_points, PLAYER2_POINTS);
}

#[test]
fn late_start_refreshes_dice_and_hub_instance_ttl() {
    let fixture = setup();

    // Establish one session so both native contract instances have the same
    // long initial retention, then move close to the end of that lifetime.
    fixture.start(70);
    let late_ledger = LEDGER + 400_000;
    ledger(&fixture.env, late_ledger);

    let dice_before = fixture
        .env
        .deployer()
        .get_contract_instance_ttl(&fixture.client.address);
    let hub_before = fixture
        .env
        .deployer()
        .get_contract_instance_ttl(&fixture.hub_address);
    assert!(dice_before < GAME_TTL_LEDGERS);
    assert!(hub_before < GAME_TTL_LEDGERS);

    fixture.start_at(71, late_ledger + 100);

    let dice_after = fixture
        .env
        .deployer()
        .get_contract_instance_ttl(&fixture.client.address);
    let hub_after = fixture
        .env
        .deployer()
        .get_contract_instance_ttl(&fixture.hub_address);
    assert!(dice_after >= GAME_TTL_LEDGERS - 1);
    assert!(hub_after >= GAME_TTL_LEDGERS - 1);
}

#[test]
fn active_game_state_uses_restorable_persistent_storage() {
    let fixture = setup();
    fixture.start(72);

    fixture.env.as_contract(&fixture.client.address, || {
        let key = DataKey::Game(72);
        assert!(fixture.env.storage().persistent().has(&key));
        assert!(!fixture.env.storage().temporary().has(&key));
    });
}

#[test]
fn start_auth_binds_every_argument_for_both_players() {
    let f = setup();
    let (commitment1, commitment2) = f.commitments(2);
    f.start_with(2, commitment1.clone(), commitment2.clone(), DEADLINE);

    let expected_args = (
        2u32,
        f.player1.clone(),
        f.player2.clone(),
        PLAYER1_POINTS,
        PLAYER2_POINTS,
        commitment1,
        commitment2,
        DEADLINE,
    )
        .into_val(&f.env);
    let expected_function = AuthorizedFunction::Contract((
        f.client.address.clone(),
        Symbol::new(&f.env, "start_game"),
        expected_args,
    ));
    let auths = f.env.auths();
    assert_eq!(auths.len(), 2);
    assert_eq!(auths[0].0, f.player1);
    assert_eq!(auths[1].0, f.player2);
    for (_, invocation) in auths {
        assert_eq!(
            invocation,
            AuthorizedInvocation {
                function: expected_function.clone(),
                sub_invocations: std::vec![],
            }
        );
    }
}

#[test]
fn wrong_secret_does_not_mutate_reveal_state() {
    let f = setup();
    f.start(3);
    let wrong = BytesN::from_array(&f.env, &[0x99; 32]);
    assert_error(
        &f.client.try_roll(&3, &f.player1, &wrong),
        Error::WrongSecret,
    );
    let game = f.client.get_game(&3);
    assert_eq!(game.phase, Phase::Revealing);
    assert!(!game.player1_rolled && !game.player2_rolled);
    assert_eq!(game.first_secret, None);
}

#[derive(Clone, Copy)]
enum CommitmentMismatch {
    Session,
    Role,
    RevealingPlayer,
    Stake,
    Contract,
}

fn assert_context_mismatch_is_rejected(mismatch: CommitmentMismatch) {
    let f = setup();
    let session_id = 40;
    let wrong_commitment = match mismatch {
        CommitmentMismatch::Session => f.commitment(
            session_id + 1,
            PLAYER1_ROLE,
            &f.player1,
            PLAYER1_POINTS,
            PLAYER2_POINTS,
            &f.secret1,
        ),
        CommitmentMismatch::Role => f.commitment(
            session_id,
            PLAYER2_ROLE,
            &f.player1,
            PLAYER1_POINTS,
            PLAYER2_POINTS,
            &f.secret1,
        ),
        CommitmentMismatch::RevealingPlayer => f.commitment(
            session_id,
            PLAYER1_ROLE,
            &f.player2,
            PLAYER1_POINTS,
            PLAYER2_POINTS,
            &f.secret1,
        ),
        CommitmentMismatch::Stake => f.commitment(
            session_id,
            PLAYER1_ROLE,
            &f.player1,
            PLAYER1_POINTS + 1,
            PLAYER2_POINTS,
            &f.secret1,
        ),
        CommitmentMismatch::Contract => {
            let admin = Address::generate(&f.env);
            let other_contract = f.env.register(DiceDuelContract, (&admin, &f.hub_address));
            DiceDuelContractClient::new(&f.env, &other_contract).commitment(
                &session_id,
                &PLAYER1_ROLE,
                &f.player1,
                &f.player1,
                &f.player2,
                &PLAYER1_POINTS,
                &PLAYER2_POINTS,
                &f.secret1,
            )
        }
    };
    let commitment2 = f.commitment(
        session_id,
        PLAYER2_ROLE,
        &f.player2,
        PLAYER1_POINTS,
        PLAYER2_POINTS,
        &f.secret2,
    );
    f.start_with(session_id, wrong_commitment, commitment2, DEADLINE);
    assert_error(
        &f.client.try_roll(&session_id, &f.player1, &f.secret1),
        Error::WrongSecret,
    );
}

#[test]
fn commitment_is_bound_to_session_role_player_stake_and_contract() {
    for mismatch in [
        CommitmentMismatch::Session,
        CommitmentMismatch::Role,
        CommitmentMismatch::RevealingPlayer,
        CommitmentMismatch::Stake,
        CommitmentMismatch::Contract,
    ] {
        assert_context_mismatch_is_rejected(mismatch);
    }
}

#[test]
fn duplicate_reveal_is_rejected_without_erasing_first_secret() {
    let f = setup();
    f.start(5);
    f.client.roll(&5, &f.player1, &f.secret1);
    assert_error(
        &f.client.try_roll(&5, &f.player1, &f.secret1),
        Error::AlreadyRolled,
    );
    let game = f.client.get_game(&5);
    assert!(game.player1_rolled);
    assert_eq!(game.first_secret, Some(f.secret1));
}

#[test]
fn reveal_deadline_is_inclusive_and_timeout_is_strictly_after() {
    let f = setup();
    f.start(6);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE);
    f.client.roll(&6, &f.player1, &f.secret1);
    f.client.roll(&6, &f.player2, &f.secret2);
    assert_eq!(f.client.get_game(&6).phase, Phase::Ready);

    let f = setup();
    f.start(7);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE);
    assert_error(
        &f.client.try_resolve_timeout(&7),
        Error::RevealDeadlineNotReached,
    );
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE + 1);
    assert_eq!(f.client.resolve_timeout(&7), None);

    let f = setup();
    f.start(8);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE + 1);
    assert_error(
        &f.client.try_roll(&8, &f.player1, &f.secret1),
        Error::RevealDeadlinePassed,
    );
}

#[test]
fn either_revealer_wins_a_one_sided_forfeit() {
    let f = setup();
    f.start(9);
    f.client.roll(&9, &f.player1, &f.secret1);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE + 1);
    assert_eq!(f.client.resolve_timeout(&9), Some(f.player1.clone()));
    assert_eq!(f.client.get_game(&9).phase, Phase::Forfeited);
    assert_eq!(f.hub.session(&9).player1_won, Some(true));

    let f = setup();
    f.start(10);
    f.client.roll(&10, &f.player2, &f.secret2);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE + 1);
    assert_eq!(f.client.resolve_timeout(&10), Some(f.player2.clone()));
    assert_eq!(f.client.get_game(&10).phase, Phase::Forfeited);
    assert_eq!(f.hub.session(&10).player1_won, Some(false));
}

#[test]
fn zero_reveal_timeout_cancels_with_exact_asymmetric_refunds() {
    let f = setup();
    f.start(11);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE + 1);
    assert_eq!(f.client.resolve_timeout(&11), None);
    let game = f.client.get_game(&11);
    assert_eq!(game.phase, Phase::Cancelled);
    assert_eq!(game.winner, None);

    let session = f.hub.session(&11);
    assert_eq!(session.status, SessionStatus::Cancelled);
    assert_eq!(session.player1_won, None);
    assert_eq!(session.player1_refund, PLAYER1_POINTS);
    assert_eq!(session.player2_refund, PLAYER2_POINTS);
}

#[test]
fn terminal_operations_are_exactly_once() {
    let f = setup();
    f.start(12);
    f.client.roll(&12, &f.player1, &f.secret1);
    f.client.roll(&12, &f.player2, &f.secret2);
    f.client.reveal_winner(&12);
    assert_error(&f.client.try_reveal_winner(&12), Error::GameAlreadyEnded);
    assert_error(
        &f.client.try_roll(&12, &f.player1, &f.secret1),
        Error::GameAlreadyEnded,
    );
    assert_error(&f.client.try_resolve_timeout(&12), Error::GameAlreadyEnded);

    let f = setup();
    f.start(13);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = DEADLINE + 1);
    f.client.resolve_timeout(&13);
    assert_error(&f.client.try_resolve_timeout(&13), Error::GameAlreadyEnded);
}

#[test]
fn active_game_uses_snapshotted_hub_after_admin_rotation() {
    let f = setup();
    f.start(14);
    let new_hub_address = f.env.register(MockGameHub, ());
    let new_hub = MockGameHubClient::new(&f.env, &new_hub_address);
    f.client.set_hub(&new_hub_address);

    f.client.roll(&14, &f.player1, &f.secret1);
    f.client.roll(&14, &f.player2, &f.secret2);
    f.client.reveal_winner(&14);
    assert_eq!(f.hub.session(&14).status, SessionStatus::Ended);
    assert!(matches!(
        new_hub.try_session(&14),
        Err(Ok(MockGameHubError::SessionNotFound))
    ));

    let (commitment1, commitment2) = f.commitments(15);
    f.start_with(15, commitment1, commitment2, DEADLINE);
    assert_eq!(new_hub.session(&15).status, SessionStatus::Active);
    assert_eq!(f.client.get_game(&15).game_hub, new_hub_address);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
enum FailingHubError {
    ForcedFailure = 1,
}

#[contracttype]
#[derive(Clone)]
enum FailingHubKey {
    Fail,
}

#[contract]
struct FailingHub;

#[contractimpl]
impl FailingHub {
    pub fn set_fail(env: Env, fail: bool) {
        env.storage().instance().set(&FailingHubKey::Fail, &fail);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_game(
        _env: Env,
        _game_id: Address,
        _session_id: u32,
        _player1: Address,
        _player2: Address,
        _player1_points: i128,
        _player2_points: i128,
    ) -> Result<(), FailingHubError> {
        Ok(())
    }

    pub fn end_game(env: Env, _session_id: u32, _player1_won: bool) -> Result<(), FailingHubError> {
        if env
            .storage()
            .instance()
            .get(&FailingHubKey::Fail)
            .unwrap_or(false)
        {
            Err(FailingHubError::ForcedFailure)
        } else {
            Ok(())
        }
    }

    pub fn cancel_game(env: Env, _session_id: u32) -> Result<(), FailingHubError> {
        if env
            .storage()
            .instance()
            .get(&FailingHubKey::Fail)
            .unwrap_or(false)
        {
            Err(FailingHubError::ForcedFailure)
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_hub_finalization_rolls_back_and_can_be_retried() {
    let env = Env::default();
    env.mock_all_auths();
    ledger(&env, LEDGER);
    let hub_address = env.register(FailingHub, ());
    let hub = FailingHubClient::new(&env, &hub_address);
    hub.set_fail(&false);
    let admin = Address::generate(&env);
    let contract = env.register(DiceDuelContract, (&admin, &hub_address));
    let client = DiceDuelContractClient::new(&env, &contract);
    let player1 = Address::generate(&env);
    let player2 = Address::generate(&env);
    let secret1 = BytesN::from_array(&env, &[0x31; 32]);
    let secret2 = BytesN::from_array(&env, &[0x32; 32]);
    let commitment1 = client.commitment(
        &16,
        &PLAYER1_ROLE,
        &player1,
        &player1,
        &player2,
        &PLAYER1_POINTS,
        &PLAYER2_POINTS,
        &secret1,
    );
    let commitment2 = client.commitment(
        &16,
        &PLAYER2_ROLE,
        &player2,
        &player1,
        &player2,
        &PLAYER1_POINTS,
        &PLAYER2_POINTS,
        &secret2,
    );
    client.start_game(
        &16,
        &player1,
        &player2,
        &PLAYER1_POINTS,
        &PLAYER2_POINTS,
        &commitment1,
        &commitment2,
        &DEADLINE,
    );
    client.roll(&16, &player1, &secret1);
    client.roll(&16, &player2, &secret2);
    let prepared = client.get_game(&16);
    assert_eq!(prepared.phase, Phase::Ready);

    hub.set_fail(&true);
    assert!(client.try_reveal_winner(&16).is_err());
    assert_eq!(client.get_game(&16), prepared);

    hub.set_fail(&false);
    let winner = client.reveal_winner(&16);
    assert_eq!(client.get_game(&16).phase, Phase::Settled);
    assert!(winner == player1 || winner == player2);
}

#[test]
fn dice_ignore_session_reveal_order_and_ledger_moment() {
    let f = setup();
    let deadline = 10_000;
    f.start_at(1001, deadline);
    f.start_at(9001, deadline);

    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = 101);
    f.client.roll(&1001, &f.player1, &f.secret1);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = 503);
    f.client.roll(&1001, &f.player2, &f.secret2);

    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = 997);
    f.client.roll(&9001, &f.player2, &f.secret2);
    f.env
        .ledger()
        .with_mut(|ledger| ledger.sequence_number = 4_001);
    f.client.roll(&9001, &f.player1, &f.secret1);

    let game_a = f.client.get_game(&1001);
    let game_b = f.client.get_game(&9001);
    assert_eq!(dice(&game_a), dice(&game_b));
    assert_eq!(game_a.winner, game_b.winner);
    assert_eq!(f.client.reveal_winner(&1001), f.client.reveal_winner(&9001));
}

#[test]
fn validation_and_legacy_error_codes_are_stable() {
    assert_eq!(Error::GameNotFound as u32, 1);
    assert_eq!(Error::NotPlayer as u32, 2);
    assert_eq!(Error::AlreadyRolled as u32, 3);
    assert_eq!(Error::BothPlayersNotRolled as u32, 4);
    assert_eq!(Error::GameAlreadyEnded as u32, 5);

    let f = setup();
    assert_error(&f.client.try_get_game(&999), Error::GameNotFound);
    let dummy = BytesN::from_array(&f.env, &[0; 32]);
    assert_error(
        &f.client.try_start_game(
            &17,
            &f.player1,
            &f.player1,
            &PLAYER1_POINTS,
            &PLAYER2_POINTS,
            &dummy,
            &dummy,
            &DEADLINE,
        ),
        Error::SamePlayer,
    );
    assert_error(
        &f.client.try_start_game(
            &17,
            &f.player1,
            &f.player2,
            &PLAYER1_POINTS,
            &PLAYER2_POINTS,
            &dummy,
            &dummy,
            &LEDGER,
        ),
        Error::InvalidDeadline,
    );
    assert_error(
        &f.client.try_start_game(
            &18,
            &f.player1,
            &f.player2,
            &PLAYER1_POINTS,
            &PLAYER2_POINTS,
            &dummy,
            &dummy,
            &(LEDGER + MAX_REVEAL_WINDOW_LEDGERS + 1),
        ),
        Error::InvalidDeadline,
    );

    f.start(19);
    let (commitment1, commitment2) = f.commitments(19);
    assert_error(
        &f.client.try_start_game(
            &19,
            &f.player1,
            &f.player2,
            &PLAYER1_POINTS,
            &PLAYER2_POINTS,
            &commitment1,
            &commitment2,
            &DEADLINE,
        ),
        Error::GameAlreadyExists,
    );
    let outsider = Address::generate(&f.env);
    assert_error(&f.client.try_roll(&19, &outsider, &dummy), Error::NotPlayer);
    assert_error(
        &f.client.try_reveal_winner(&19),
        Error::BothPlayersNotRolled,
    );
}

#[test]
fn upgrade_entrypoint_keeps_admin_guarded_shape() {
    let f = setup();
    assert_eq!(f.client.get_admin(), f.admin);
    let missing_wasm = BytesN::from_array(&f.env, &[0; 32]);
    assert!(f.client.try_upgrade(&missing_wasm).is_err());
}
