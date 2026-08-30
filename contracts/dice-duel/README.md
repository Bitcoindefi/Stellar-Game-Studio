# Dice Duel

`dice-duel` is a two-player Soroban game that uses commit/reveal so neither a
caller nor the contract can choose a winning public input after seeing the
other player's contribution.

## Protocol

Each player generates a uniformly random `BytesN<32>` secret off chain. A
commitment is SHA-256 over a canonical Soroban `SCV_MAP` XDR value containing
the protocol version and domain, this Dice Duel contract address, the
`dice-duel` game tag, session ID, player role and address, both player
addresses, both exact stakes, and the secret. A separate salt is unnecessary:
a uniformly random 256-bit secret already supplies the required entropy.

Both commitments, both complete player authorizations, and the reveal deadline
are supplied to `start_game` before the Hub locks either stake. Each player then
reveals with `roll`. Reveals are accepted through the deadline inclusively. The
four dice are derived in fixed player-role order from both verified secrets;
they do not depend on reveal order, ledger time, or `session_id`.

After both reveals, anyone may call `reveal_winner`. Ties go to player 1. After
the deadline, anyone may call `resolve_timeout`: exactly one revealer wins by
forfeit, while zero reveals cause a neutral Hub cancellation and exact refund
of each player's asymmetric stake. Terminal Hub interaction is guarded by the
`Finalizing` phase and Soroban transaction rollback, so a failed Hub call can
be retried without leaving a partially finalized game.

Game records use persistent storage with a 30-day live TTL. An unattended game
is therefore archived rather than deleted irreversibly. If it has archived,
restore the Dice game entry and required contract/Hub footprint before calling
`reveal_winner`, `resolve_timeout`, or `get_game`.

## ABI

### `commitment`

Computes the commitment for a player. Production clients should reproduce this
canonical encoding and hash locally, using the golden vector in the test suite,
and retain the secret securely until reveal. **Never sign or submit a
`commitment` invocation as a transaction, and never send the secret to an
untrusted RPC.** Trusted local simulation is suitable only as a development
cross-check.

```text
commitment(
    session_id: u32,
    role: u32, // 1 = player 1, 2 = player 2
    revealing_player: Address,
    player1: Address,
    player2: Address,
    player1_points: i128,
    player2_points: i128,
    secret: BytesN<32>,
) -> BytesN<32>
```

### `start_game`

```text
start_game(
    session_id: u32,
    player1: Address,
    player2: Address,
    player1_points: i128,
    player2_points: i128,
    player1_commitment: BytesN<32>,
    player2_commitment: BytesN<32>,
    reveal_deadline: u32,
) -> Result<(), Error>
```

Both players authorize the complete argument set. The deadline must be in the
future and fit within the persistent entry's 30-day live TTL while preserving
the timeout-resolution grace window.

### `roll`

```text
roll(session_id: u32, player: Address, secret: BytesN<32>)
    -> Result<(), Error>
```

Requires the revealing player's authorization. It rejects a wrong secret, a
duplicate reveal, and a reveal after the deadline.

### `reveal_winner`

```text
reveal_winner(session_id: u32) -> Result<Address, Error>
```

Permissionless. It settles a game only after both valid reveals.

### `resolve_timeout`

```text
resolve_timeout(session_id: u32) -> Result<Option<Address>, Error>
```

Permissionless and available only after the deadline. Returns the sole
revealer for a forfeit or `None` for neutral cancellation.

### Queries and administration

- `get_game(session_id)` returns the complete stored game state.
- `get_admin()` and `get_hub()` return current configuration.
- `set_admin(new_admin)`, `set_hub(new_hub)`, and `upgrade(new_wasm_hash)`
  require administrator authorization.

Each game snapshots its Hub address at creation. Changing the configured Hub
therefore affects only later games.

## Errors

Codes 1 through 5 are the legacy Dice Duel ABI and remain stable:

1. `GameNotFound`
2. `NotPlayer`
3. `AlreadyRolled`
4. `BothPlayersNotRolled`
5. `GameAlreadyEnded`

New errors use codes above the preserved range:

6. `SamePlayer`
7. `GameAlreadyExists`
8. `InvalidDeadline`
9. `RevealDeadlinePassed`
10. `RevealDeadlineNotReached`
11. `WrongSecret`
12. `FinalizationInProgress`

## Build and test

```bash
cargo test --workspace --locked -j 2
cargo build --locked --release --target wasm32v1-none -p dice-duel -p mock-game-hub
cargo test -p dice-duel --test wasm_resource --locked -- --ignored --nocapture
```

The ignored resource test loads the optimized Wasm files at runtime and checks
normal settlement, one-sided forfeit, and zero-reveal cancellation under the
Soroban SDK's mainnet invocation limits.

## Deployment gates

The repository mock demonstrates the proposed neutral `cancel_game(session_id)`
behavior; it is not evidence of production Hub settlement economics. Before
deployment, the production Hub must confirm that ABI and exact refund
semantics. Because the game state and public ABI changed, deploy a new Dice
contract ID (or prove all legacy sessions are drained), then regenerate Studio
bindings and add secure client-side secret retention and timeout UI. This
change does not update the Studio contract ID or generated TypeScript bindings.
