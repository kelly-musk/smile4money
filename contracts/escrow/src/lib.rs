//! # Escrow Contract
//!
//! Trustless chess-match escrow on Stellar Soroban. Players stake XLM or USDC before a game;
//! the verified winner is paid out automatically the moment the oracle submits the result.
//!
//! ## State Machine
//!
//! Every match moves through the following states. Invalid transitions are rejected on-chain
//! with [`Error::InvalidState`].
//!
//! ```text
//! (none) ──create_match()──► Pending
//!                                │
//!                ┌───────────────┤
//!                │               │
//!         cancel_match()    deposit() × 2
//!                │               │
//!                ▼               ▼
//!            Cancelled        Active ──── claim_timeout() ──► Cancelled
//!                           (funds held)
//!                                │
//!                         submit_result()
//!                          (oracle only)
//!                                │
//!                                ▼
//!                         PendingResult  ◄── override_result() (admin)
//!                          (dispute window)
//!                                │
//!                       finalize_result()
//!                     (after window expires)
//!                                │
//!                                ▼
//!                           Completed
//!                          (payout done)
//! ```
//!
//! `Cancelled` and `Completed` are **terminal** — no further transitions are possible.
//!
//! ## Key Data Structures
//!
//! - [`Match`](types::Match) — full record of a single betting match stored in persistent storage.
//! - [`MatchState`](types::MatchState) — lifecycle enum driving the state machine above.
//! - [`Winner`](types::Winner) — payout outcome: `Player1`, `Player2`, or `Draw`.
//! - [`Platform`](types::Platform) — chess platform the game is hosted on (`Lichess` / `ChessDotCom`).
//! - [`DataKey`](types::DataKey) — all storage keys used by the contract.
//! - [`Error`](errors::Error) — every error code the contract can return.
//!
//! ## Further Reading
//!
//! - Architecture & sequence diagrams: [`docs/architecture.md`](../../docs/architecture.md)
//! - Full API reference with CLI examples: [`docs/api-reference.md`](../../docs/api-reference.md)
//! - Emergency procedures: [`docs/runbook.md`](../../docs/runbook.md)

#![no_std]

mod errors;
mod types;

use errors::Error;
use soroban_sdk::{contract, contractimpl, symbol_short, token, vec, Address, Env, String, Symbol, Vec};
use types::{DataKey, Match, MatchState, OptionalWinner, Platform, Winner};

/// ~30 days at 5s/ledger. Used as both the TTL threshold and the extend-to value.
const MATCH_TTL_LEDGERS: u32 = 518_400;

/// Maximum allowed byte length for a game_id string.
const MAX_GAME_ID_LEN: u32 = 64;

/// Dispute window: ~24 hours at 5s/ledger (17 280 ledgers).
/// After an oracle result is submitted, the admin has this many ledgers to call
/// `override_result` before the result is finalised and payout is executed.
const DISPUTE_WINDOW_LEDGERS: u32 = 17_280;

/// Match timeout: ~7 days at 5s/ledger (120 960 ledgers).
/// If a match has been `Active` for longer than this many ledgers without an oracle
/// result, either player may call `claim_timeout` to reclaim their stake.
const TIMEOUT_LEDGERS: u32 = 120_960;

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    fn is_paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    fn get_match_count(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MatchCount)
            .unwrap_or(0)
    }

    fn validate_match_id(env: &Env, match_id: u64) -> Result<(), Error> {
        if match_id >= Self::get_match_count(env) {
            return Err(Error::MatchNotFound);
        }
        Ok(())
    }

    /// Initialize the contract with a trusted oracle address, an admin, and a default token.
    ///
    /// # Panics
    ///
    /// Panics with `"Contract already initialized"` if called more than once.
    pub fn initialize(
        env: Env,
        oracle: Address,
        admin: Address,
        token: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Oracle) {
            panic!("Contract already initialized");
        }
        let token_client = token::Client::new(&env, &token);
        let _ = token_client.decimals();
        env.storage().instance().set(&DataKey::Oracle, &oracle);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::MatchCount, &0u64);
        env.storage().instance().set(&DataKey::Paused, &false);
        Ok(())
    }

    /// Rotate the oracle address — requires the current admin to authorize.
    pub fn update_oracle(env: Env, new_oracle: Address) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::Unauthorized)?;
        admin.require_auth();
        let old_oracle: Address = env
            .storage()
            .instance()
            .get(&DataKey::Oracle)
            .ok_or(Error::Unauthorized)?;
        env.storage().instance().set(&DataKey::Oracle, &new_oracle);
        env.events().publish(
            (
                Symbol::new(&env, "admin"),
                Symbol::new(&env, "oracle_updated"),
            ),
            (old_oracle, new_oracle, admin),
        );
        Ok(())
    }

    /// Rotate the admin address — requires the current admin to authorize.
    pub fn transfer_admin(env: Env, caller: Address, new_admin: Address) -> Result<(), Error> {
        let current_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::Unauthorized)?;

        if caller != current_admin {
            return Err(Error::Unauthorized);
        }
        caller.require_auth();

        if new_admin.to_string()
            == String::from_str(
                &env,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            )
            || new_admin == current_admin
        {
            return Err(Error::InvalidAdmin);
        }

        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.events().publish(
            (Symbol::new(&env, "admin"), symbol_short!("transfer")),
            (current_admin, new_admin),
        );
        Ok(())
    }

    /// Pause the contract — admin only. Blocks create_match, deposit, and submit_result.
    pub fn pause(env: Env) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::Unauthorized)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish(
            (Symbol::new(&env, "admin"), symbol_short!("paused")),
            (admin, env.ledger().sequence()),
        );
        Ok(())
    }

    /// Unpause the contract — admin only.
    pub fn unpause(env: Env) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::Unauthorized)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &false);
        env.events().publish(
            (Symbol::new(&env, "admin"), symbol_short!("unpaused")),
            (admin, env.ledger().sequence()),
        );
        Ok(())
    }

    /// Create a new match. Both players must call `deposit` before the game starts.
    pub fn create_match(
        env: Env,
        player1: Address,
        player2: Address,
        stake_amount: i128,
        token: Address,
        game_id: String,
        platform: Platform,
    ) -> Result<u64, Error> {
        player1.require_auth();

        if Self::is_paused(&env) {
            return Err(Error::ContractPaused);
        }
        if stake_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if player1 == player2 {
            return Err(Error::InvalidPlayers);
        }
        let game_id_len = game_id.len();
        if game_id_len == 0 || game_id_len > MAX_GAME_ID_LEN {
            return Err(Error::InvalidGameId);
        }
        if env
            .storage()
            .persistent()
            .has(&DataKey::GameId(game_id.clone()))
        {
            return Err(Error::DuplicateGameId);
        }

        let stored_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::Unauthorized)?;
        if token != stored_token {
            return Err(Error::InvalidToken);
        }

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MatchCount)
            .unwrap_or(0);

        if env.storage().persistent().has(&DataKey::Match(id)) {
            return Err(Error::AlreadyExists);
        }

        let m = Match {
            id,
            player1,
            player2,
            stake_amount,
            token,
            game_id,
            platform,
            state: MatchState::Pending,
            player1_deposited: false,
            player2_deposited: false,
            created_ledger: env.ledger().sequence(),
            activated_ledger: 0,
            pending_result_ledger: 0,
            pending_winner: OptionalWinner::None,
            cancelled_ledger: None,
            completed_ledger: None,
        };

        env.storage().persistent().set(&DataKey::Match(id), &m);
        env.storage().persistent().extend_ttl(
            &DataKey::Match(id),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );
        env.storage()
            .persistent()
            .set(&DataKey::GameId(m.game_id.clone()), &id);
        env.storage().persistent().extend_ttl(
            &DataKey::GameId(m.game_id.clone()),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );
        let next_id = id.checked_add(1).ok_or(Error::Overflow)?;
        env.storage().instance().set(&DataKey::MatchCount, &next_id);

        env.events().publish(
            (Symbol::new(&env, "match"), symbol_short!("created")),
            (id, m.player1, m.player2, stake_amount, m.game_id),
        );

        Ok(id)
    }

    /// Player deposits their stake into escrow.
    pub fn deposit(env: Env, match_id: u64, player: Address) -> Result<(), Error> {
        player.require_auth();

        if Self::is_paused(&env) {
            return Err(Error::ContractPaused);
        }

        Self::validate_match_id(&env, match_id)?;

        let mut m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;

        if m.state == MatchState::Cancelled {
            return Err(Error::MatchCancelled);
        }
        if m.state == MatchState::Completed {
            return Err(Error::MatchCompleted);
        }
        if m.state != MatchState::Pending {
            return Err(Error::InvalidState);
        }

        let is_p1 = player == m.player1;
        let is_p2 = player == m.player2;

        if !is_p1 && !is_p2 {
            return Err(Error::Unauthorized);
        }

        let already_deposited = if is_p1 {
            m.player1_deposited
        } else {
            m.player2_deposited
        };

        if already_deposited {
            return Err(Error::AlreadyFunded);
        }

        let client = token::Client::new(&env, &m.token);
        let allowance = client.allowance(&player, &env.current_contract_address());
        if allowance < m.stake_amount {
            return Err(Error::InsufficientAllowance);
        }
        client
            .try_transfer(&player, &env.current_contract_address(), &m.stake_amount)
            .map_err(|_| Error::TransferFailed)?
            .map_err(|_| Error::TransferFailed)?;

        if is_p1 {
            m.player1_deposited = true;
        } else {
            m.player2_deposited = true;
        }

        if m.player1_deposited && m.player2_deposited {
            // STATE TRANSITION: Pending → Active
            // Record the ledger at which the match became active for timeout tracking.
            m.state = MatchState::Active;
            m.activated_ledger = env.ledger().sequence();
            env.events().publish(
                (Symbol::new(&env, "match"), symbol_short!("activated")),
                match_id,
            );
        }

        let player_label = if is_p1 {
            symbol_short!("player1")
        } else {
            symbol_short!("player2")
        };
        env.events().publish(
            (Symbol::new(&env, "match"), symbol_short!("deposit")),
            (match_id, player, m.stake_amount, player_label),
        );

        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id), &m);
        env.storage().persistent().extend_ttl(
            &DataKey::Match(match_id),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );

        Ok(())
    }

    /// Oracle submits the verified match result. Transitions match to `PendingResult`
    /// and starts the dispute window. Payout is NOT executed immediately — call
    /// `finalize_result` after `DISPUTE_WINDOW_LEDGERS` to execute the payout.
    ///
    /// `game_id` must match the game_id stored in the match to prevent cross-match
    /// result injection.
    pub fn submit_result(
        env: Env,
        match_id: u64,
        game_id: String,
        winner: Winner,
        caller: Address,
    ) -> Result<(), Error> {
        if Self::is_paused(&env) {
            return Err(Error::ContractPaused);
        }

        let oracle: Address = env
            .storage()
            .instance()
            .get(&DataKey::Oracle)
            .ok_or(Error::Unauthorized)?;

        if caller != oracle {
            return Err(Error::Unauthorized);
        }
        caller.require_auth();

        Self::validate_match_id(&env, match_id)?;

        let mut m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;

        if m.game_id != game_id {
            return Err(Error::GameIdMismatch);
        }

        if m.state != MatchState::Active {
            return Err(Error::InvalidState);
        }

        if !m.player1_deposited || !m.player2_deposited {
            return Err(Error::NotFunded);
        }

        // STATE TRANSITION: Active → PendingResult
        // The oracle's result enters a dispute window. No payout yet.
        m.state = MatchState::PendingResult;
        m.pending_result_ledger = env.ledger().sequence();
        m.pending_winner = OptionalWinner::Some(winner.clone());

        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id), &m);
        env.storage().persistent().extend_ttl(
            &DataKey::Match(match_id),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );

        env.events().publish(
            (Symbol::new(&env, "oracle"), symbol_short!("pending")),
            (match_id, winner, m.pending_result_ledger),
        );

        Ok(())
    }

    /// Admin overrides an oracle result during the dispute window.
    ///
    /// Can only be called while the match is in `PendingResult` state and before
    /// `DISPUTE_WINDOW_LEDGERS` have elapsed since the result was submitted.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`]      — caller is not the admin.
    /// * [`Error::InvalidState`]      — match is not in `PendingResult` state.
    /// * [`Error::DisputeWindowActive`] — dispute window has already expired; call
    ///   `finalize_result` instead.
    pub fn override_result(
        env: Env,
        match_id: u64,
        new_winner: Winner,
        caller: Address,
    ) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::Unauthorized)?;

        if caller != admin {
            return Err(Error::Unauthorized);
        }
        caller.require_auth();

        Self::validate_match_id(&env, match_id)?;

        let mut m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;

        if m.state != MatchState::PendingResult {
            return Err(Error::InvalidState);
        }

        // Ensure the dispute window has not yet expired; after expiry the result
        // is final and must be processed via finalize_result.
        let current = env.ledger().sequence();
        if current > m.pending_result_ledger + DISPUTE_WINDOW_LEDGERS {
            return Err(Error::DisputeWindowActive);
        }

        let old_winner = m.pending_winner.clone();
        m.pending_winner = OptionalWinner::Some(new_winner.clone());

        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id), &m);
        env.storage().persistent().extend_ttl(
            &DataKey::Match(match_id),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );

        env.events().publish(
            (Symbol::new(&env, "oracle"), symbol_short!("overridn")),
            (match_id, old_winner, new_winner),
        );

        Ok(())
    }

    /// Finalize a pending result and execute payout after the dispute window has expired.
    ///
    /// Can be called by anyone once `DISPUTE_WINDOW_LEDGERS` have elapsed since the oracle
    /// submitted the result. Executes the payout based on `pending_winner` and transitions
    /// the match to `Completed`.
    ///
    /// # Errors
    ///
    /// * [`Error::InvalidState`]        — match is not in `PendingResult` state.
    /// * [`Error::DisputeWindowActive`] — dispute window has not yet expired.
    pub fn finalize_result(env: Env, match_id: u64) -> Result<(), Error> {
        Self::validate_match_id(&env, match_id)?;

        let mut m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;

        if m.state != MatchState::PendingResult {
            return Err(Error::InvalidState);
        }

        let current = env.ledger().sequence();
        if current <= m.pending_result_ledger + DISPUTE_WINDOW_LEDGERS {
            return Err(Error::DisputeWindowActive);
        }

        let winner = match m.pending_winner.clone() {
            OptionalWinner::Some(w) => w,
            OptionalWinner::None => return Err(Error::InvalidState),
        };

        let client = token::Client::new(&env, &m.token);

        let payout_amount: i128 = match winner {
            Winner::Draw => m.stake_amount,
            _ => m.stake_amount * 2,
        };

        match winner.clone() {
            Winner::Player1 => {
                client.transfer(&env.current_contract_address(), &m.player1, &payout_amount)
            }
            Winner::Player2 => {
                client.transfer(&env.current_contract_address(), &m.player2, &payout_amount)
            }
            Winner::Draw => {
                client.transfer(&env.current_contract_address(), &m.player1, &payout_amount);
                client.transfer(&env.current_contract_address(), &m.player2, &payout_amount);
            }
        }

        // STATE TRANSITION: PendingResult → Completed
        m.state = MatchState::Completed;
        m.completed_ledger = Some(env.ledger().sequence());
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id), &m);
        env.storage().persistent().extend_ttl(
            &DataKey::Match(match_id),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );

        env.events().publish(
            (Symbol::new(&env, "match"), symbol_short!("completed")),
            (match_id, winner, payout_amount),
        );

        Ok(())
    }

    /// Reclaim funds when the oracle never submits a result within `TIMEOUT_LEDGERS`.
    ///
    /// Either `player1` or `player2` may call this function if the match has been in the
    /// `Active` state for longer than `TIMEOUT_LEDGERS` (~7 days) without an oracle result.
    /// Both players receive their original `stake_amount` back. The match transitions to
    /// `Cancelled`.
    ///
    /// # Arguments
    ///
    /// * `match_id` — The match to reclaim funds from.
    /// * `caller`   — Must be `player1` or `player2`; must authorize the call.
    ///
    /// # Errors
    ///
    /// * [`Error::Unauthorized`]  — caller is neither player.
    /// * [`Error::InvalidState`]  — match is not `Active`.
    /// * [`Error::MatchTimedOut`] — not enough ledgers have passed yet (too early to claim).
    pub fn claim_timeout(env: Env, match_id: u64, caller: Address) -> Result<(), Error> {
        caller.require_auth();

        Self::validate_match_id(&env, match_id)?;

        let mut m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;

        if m.state != MatchState::Active {
            return Err(Error::InvalidState);
        }

        let is_player = caller == m.player1 || caller == m.player2;
        if !is_player {
            return Err(Error::Unauthorized);
        }

        let current = env.ledger().sequence();
        if current <= m.activated_ledger + TIMEOUT_LEDGERS {
            // Timeout period has not elapsed yet — reject with MatchTimedOut reused
            // as "too early". We return MatchTimedOut here to keep error codes minimal;
            // callers should interpret it as "timeout not yet reached".
            return Err(Error::MatchTimedOut);
        }

        let client = token::Client::new(&env, &m.token);
        // Refund both players their original stake
        client.transfer(&env.current_contract_address(), &m.player1, &m.stake_amount);
        client.transfer(&env.current_contract_address(), &m.player2, &m.stake_amount);

        // STATE TRANSITION: Active → Cancelled (via timeout)
        m.state = MatchState::Cancelled;
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id), &m);
        env.storage().persistent().extend_ttl(
            &DataKey::Match(match_id),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );

        env.events().publish(
            (Symbol::new(&env, "match"), symbol_short!("timeout")),
            (match_id, caller),
        );

        Ok(())
    }

    /// Cancel a pending match and refund any deposits.
    ///
    /// Authorization model:
    /// - If neither or only one player has deposited: the calling player's auth suffices.
    /// - If both players have deposited: both players must authorize, because cancelling
    ///   would withdraw funds that the other player has already committed.
    ///
    /// Cancelation is allowed while the contract is paused so players can recover funds.
    pub fn cancel_match(env: Env, match_id: u64, caller: Address) -> Result<(), Error> {
        Self::validate_match_id(&env, match_id)?;

        let mut m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;

        if m.state != MatchState::Pending {
            return Err(Error::InvalidState);
        }

        let is_p1 = caller == m.player1;
        let is_p2 = caller == m.player2;

        if !is_p1 && !is_p2 {
            return Err(Error::Unauthorized);
        }

        if m.player1_deposited && m.player2_deposited {
            m.player1.require_auth();
            m.player2.require_auth();
        } else {
            caller.require_auth();
        }

        let client = token::Client::new(&env, &m.token);
        let player1_refund: i128 = if m.player1_deposited { m.stake_amount } else { 0 };
        let player2_refund: i128 = if m.player2_deposited { m.stake_amount } else { 0 };
        if m.player1_deposited {
            client
                .try_transfer(&env.current_contract_address(), &m.player1, &m.stake_amount)
                .map_err(|_| Error::TransferFailed)?
                .map_err(|_| Error::TransferFailed)?;
        }
        if m.player2_deposited {
            client
                .try_transfer(&env.current_contract_address(), &m.player2, &m.stake_amount)
                .map_err(|_| Error::TransferFailed)?
                .map_err(|_| Error::TransferFailed)?;
        }

        // STATE TRANSITION: Pending → Cancelled
        m.state = MatchState::Cancelled;
        m.cancelled_ledger = Some(env.ledger().sequence());
        env.storage()
            .persistent()
            .set(&DataKey::Match(match_id), &m);
        env.storage().persistent().extend_ttl(
            &DataKey::Match(match_id),
            MATCH_TTL_LEDGERS,
            MATCH_TTL_LEDGERS,
        );

        env.events().publish(
            (Symbol::new(&env, "match"), symbol_short!("cancelled")),
            (match_id, caller, player1_refund, player2_refund),
        );

        Ok(())
    }

    /// Drain all token holdings to a safe address — admin only, requires contract to be paused.
    pub fn emergency_drain(env: Env, to: Address, caller: Address) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::Unauthorized)?;

        if caller != admin {
            return Err(Error::Unauthorized);
        }
        caller.require_auth();

        if !Self::is_paused(&env) {
            return Err(Error::NotPaused);
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::Unauthorized)?;

        let client = token::Client::new(&env, &token);
        let contract = env.current_contract_address();
        let balance = client.balance(&contract);

        if balance > 0 {
            client.transfer(&contract, &to, &balance);
        }

        env.events().publish(
            (Symbol::new(&env, "admin"), symbol_short!("drain")),
            (balance, to, admin),
        );

        Ok(())
    }

    /// Read a match by ID.
    pub fn get_match(env: Env, match_id: u64) -> Result<Match, Error> {
        Self::validate_match_id(&env, match_id)?;
        env.storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)
    }

    /// Check whether both players have deposited.
    pub fn is_funded(env: Env, match_id: u64) -> Result<bool, Error> {
        let m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;
        Ok(m.player1_deposited && m.player2_deposited)
    }

    /// Return the total escrowed balance for a match (0, 1x, or 2x stake).
    pub fn get_escrow_balance(env: Env, match_id: u64) -> Result<i128, Error> {
        let m: Match = env
            .storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .ok_or(Error::MatchNotFound)?;
        if m.state == MatchState::Completed || m.state == MatchState::Cancelled {
            return Ok(0);
        }
        let deposited: i128 = match (m.player1_deposited, m.player2_deposited) {
            (true, true) => 2,
            (true, false) | (false, true) => 1,
            (false, false) => 0,
        };
        Ok(deposited * m.stake_amount)
    }

    /// Return a page of match IDs in the range `[start, start + limit)`.
    ///
    /// `limit` is capped at 100. IDs beyond the current match count are silently
    /// omitted, so callers can detect the last page when the returned slice is
    /// shorter than the requested `limit`.
    pub fn list_matches(env: Env, start: u64, limit: u32) -> Vec<u64> {
        const MAX_LIMIT: u32 = 100;
        let limit = limit.min(MAX_LIMIT);
        let count = Self::get_match_count(&env);
        let mut ids: Vec<u64> = vec![&env];
        let end = start.saturating_add(limit as u64).min(count);
        let mut i = start;
        while i < end {
            ids.push_back(i);
            i += 1;
        }
        ids
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_e2e;
