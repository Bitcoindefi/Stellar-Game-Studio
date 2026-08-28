use dice_duel::{DiceDuelContract, DiceDuelContractClient};
use sha2::{Digest, Sha256};
use soroban_sdk::{Address, BytesN, Env, TryFromVal};
use stellar_xdr::{
    AccountId, ContractId, Hash, Limits, PublicKey, ScAddress, ScBytes, ScMap, ScSymbol, ScVal,
    Uint256, WriteXdr,
};

const EXPECTED_XDR_LEN: usize = 540;
const EXPECTED_SHA256: [u8; 32] = [
    0x03, 0x7c, 0x35, 0x6c, 0x8a, 0x9b, 0xab, 0xe4, 0x16, 0x2a, 0x94, 0x3e, 0xb1, 0xc2, 0x93, 0xbb,
    0xf2, 0x97, 0xac, 0xe9, 0xb4, 0x27, 0xa3, 0x3e, 0x60, 0x51, 0x2c, 0xa9, 0x25, 0x00, 0xd4, 0x9b,
];

fn account(env: &Env, byte: u8) -> Address {
    Address::try_from_val(
        env,
        &ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(
            [byte; 32],
        )))),
    )
    .unwrap()
}

fn contract(env: &Env, byte: u8) -> Address {
    Address::try_from_val(env, &ScAddress::Contract(ContractId(Hash([byte; 32])))).unwrap()
}

fn symbol(value: &str) -> ScVal {
    ScVal::Symbol(ScSymbol(value.as_bytes().to_vec().try_into().unwrap()))
}

fn bytes(value: &[u8]) -> ScVal {
    ScVal::Bytes(ScBytes(value.to_vec().try_into().unwrap()))
}

fn address(value: &Address) -> ScVal {
    ScVal::Address(value.into())
}

#[allow(clippy::too_many_arguments)]
fn independent_commitment_xdr(
    contract_id: &Address,
    session_id: u32,
    role: u32,
    revealing_player: &Address,
    player1: &Address,
    player2: &Address,
    player1_points: i128,
    player2_points: i128,
    secret: [u8; 32],
) -> Vec<u8> {
    // Construct the raw SCV_MAP with stellar-xdr, independently of the
    // contract's Rust type and its `ToXdr` implementation call site.
    let map = ScMap::sorted_from(vec![
        (
            symbol("domain"),
            bytes(b"stellar-game-studio:commitment:v1"),
        ),
        (symbol("contract"), address(contract_id)),
        (symbol("game"), bytes(b"dice-duel")),
        (symbol("session_id"), ScVal::U32(session_id)),
        (symbol("role"), ScVal::U32(role)),
        (symbol("revealing_player"), address(revealing_player)),
        (symbol("player1"), address(player1)),
        (symbol("player2"), address(player2)),
        (symbol("player1_points"), ScVal::from(player1_points)),
        (symbol("player2_points"), ScVal::from(player2_points)),
        (symbol("secret"), bytes(&secret)),
    ])
    .unwrap();
    ScVal::Map(Some(map)).to_xdr(Limits::none()).unwrap()
}

fn hex(input: &[u8]) -> String {
    input.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn canonical_scv_map_xdr_sha256_golden_vector() {
    let env = Env::default();
    let admin = account(&env, 0xa1);
    let hub = contract(&env, 0xb2);
    let dice = contract(&env, 0xc3);
    env.register_at(&dice, DiceDuelContract, (&admin, &hub));
    let client = DiceDuelContractClient::new(&env, &dice);

    let player1 = account(&env, 0x11);
    let player2 = account(&env, 0x22);
    let secret = [0x5a; 32];
    let xdr = independent_commitment_xdr(
        &dice,
        0x0102_0304,
        1,
        &player1,
        &player1,
        &player2,
        1_234_567_890_123,
        9_876_543_210_987,
        secret,
    );
    let digest: [u8; 32] = Sha256::digest(&xdr).into();

    // These literals make this a real golden vector rather than two dynamic
    // implementations that could accidentally drift together.
    assert_eq!(xdr.len(), EXPECTED_XDR_LEN);
    assert_eq!(digest, EXPECTED_SHA256);

    let actual = client.commitment(
        &0x0102_0304,
        &1,
        &player1,
        &player1,
        &player2,
        &1_234_567_890_123,
        &9_876_543_210_987,
        &BytesN::from_array(&env, &secret),
    );

    println!("COMMITMENT_VECTOR xdr_len={} xdr={}", xdr.len(), hex(&xdr));
    println!("COMMITMENT_VECTOR sha256={}", hex(&digest));
    assert_eq!(actual.to_array(), digest);
}
