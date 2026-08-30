use dice_duel::DiceDuelContractClient;
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger as _};
use soroban_sdk::{Address, BytesN, Env};
use std::path::{Path, PathBuf};

const MAINNET_CPU_LIMIT: u64 = 400_000_000;
const MAINNET_MEMORY_LIMIT: u64 = 41_943_040;

fn optimized_wasm(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target/wasm32v1-none/release")
        .join(name)
}

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 1_700_000_000,
        protocol_version: 27,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 100_000,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 1_000_000,
    });
    env
}

fn wasm_fixture(
    session_id: u32,
) -> (
    Env,
    DiceDuelContractClient<'static>,
    Address,
    Address,
    BytesN<32>,
    BytesN<32>,
    u32,
) {
    let dice_path = optimized_wasm("dice_duel.wasm");
    let hub_path = optimized_wasm("mock_game_hub.wasm");
    let dice_wasm = std::fs::read(&dice_path)
        .unwrap_or_else(|e| panic!("read optimized {}: {e}", dice_path.display()));
    let hub_wasm = std::fs::read(&hub_path)
        .unwrap_or_else(|e| panic!("read optimized {}: {e}", hub_path.display()));

    let env = test_env();
    let hub = env.register(hub_wasm.as_slice(), ());
    let admin = Address::generate(&env);
    let dice = env.register(dice_wasm.as_slice(), (&admin, &hub));
    let client = DiceDuelContractClient::new(&env, &dice);
    let player1 = Address::generate(&env);
    let player2 = Address::generate(&env);
    let secret1 = BytesN::from_array(&env, &[0x11; 32]);
    let secret2 = BytesN::from_array(&env, &[0x22; 32]);
    let player1_points = 17i128;
    let player2_points = 29i128;
    let deadline = 200u32;
    let commitment1 = client.commitment(
        &session_id,
        &1,
        &player1,
        &player1,
        &player2,
        &player1_points,
        &player2_points,
        &secret1,
    );
    let commitment2 = client.commitment(
        &session_id,
        &2,
        &player2,
        &player1,
        &player2,
        &player1_points,
        &player2_points,
        &secret2,
    );
    client.start_game(
        &session_id,
        &player1,
        &player2,
        &player1_points,
        &player2_points,
        &commitment1,
        &commitment2,
        &deadline,
    );
    (env, client, player1, player2, secret1, secret2, deadline)
}

fn report_last_invocation(env: &Env, path: &str) {
    // Capture immediately: any getter would replace the last top-level sample.
    let resources = env.cost_estimate().resources();
    let budget = env.cost_estimate().budget();
    let cpu = budget.cpu_instruction_cost();
    let memory = budget.memory_bytes_cost();
    println!("WASM_RESOURCE path={path} cpu={cpu} memory={memory} resources={resources:?}");
    assert!(cpu < MAINNET_CPU_LIMIT, "{path} CPU budget exceeded");
    assert!(
        memory < MAINNET_MEMORY_LIMIT,
        "{path} memory budget exceeded"
    );
}

#[test]
#[ignore = "requires freshly built optimized dice-duel and mock-game-hub Wasm"]
fn optimized_wasm_terminal_paths_fit_mainnet_invocation_limits() {
    let (env, client, player1, player2, secret1, secret2, _) = wasm_fixture(7001);
    client.roll(&7001, &player1, &secret1);
    client.roll(&7001, &player2, &secret2);
    client.reveal_winner(&7001);
    report_last_invocation(&env, "normal_settlement");

    let (env, client, player1, _player2, secret1, _secret2, deadline) = wasm_fixture(7002);
    client.roll(&7002, &player1, &secret1);
    env.ledger()
        .with_mut(|ledger| ledger.sequence_number = deadline + 1);
    client.resolve_timeout(&7002);
    report_last_invocation(&env, "one_sided_forfeit");

    let (env, client, _player1, _player2, _secret1, _secret2, deadline) = wasm_fixture(7003);
    env.ledger()
        .with_mut(|ledger| ledger.sequence_number = deadline + 1);
    client.resolve_timeout(&7003);
    report_last_invocation(&env, "zero_reveal_cancel");
}
