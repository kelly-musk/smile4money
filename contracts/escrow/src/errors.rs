use soroban_sdk::contracterror;

/// Errors returned by the escrow contract.
///
/// Each variant carries a stable numeric code (the discriminant) that is
/// encoded on-chain and surfaced to clients. Do **not** renumber existing
/// variants — doing so is a breaking change for any client that inspects the
/// raw error code.
///
/// # Error code table
///
/// | Code | Variant            | Meaning                                              |
/// |------|--------------------|------------------------------------------------------|
/// |  1   | MatchNotFound      | No match exists for the given match_id               |
/// |  2   | AlreadyFunded      | The calling player has already deposited for this match |
/// |  3   | NotFunded          | submit_result called before both players deposited   |
/// |  4   | Unauthorized       | Caller is not permitted to perform this action       |
/// |  5   | InvalidState       | Operation is not valid in the match's current state  |
/// |  6   | AlreadyExists      | A match with this ID already exists (counter collision) |
/// |  7   | AlreadyInitialized | Contract has already been initialized                |
/// |  8   | Overflow           | Match ID counter would overflow u64                  |
/// |  9   | ContractPaused     | Contract is paused; mutating operations are blocked  |
/// | 10   | InvalidAmount      | stake_amount must be greater than zero               |
/// | 11   | InvalidGameId      | game_id is empty or exceeds the 64-byte maximum      |
/// | 12   | InvalidPlayers     | player1 and player2 must be different addresses      |
/// | 13   | GameIdMismatch     | Oracle submitted a result for the wrong game_id      |
/// | 14   | DuplicateGameId    | game_id is already linked to another match           |
/// | 15   | TransferFailed     | token transfer failed                                |
/// | 16   | MatchCancelled     | deposit rejected: match has been cancelled           |
/// | 17   | MatchCompleted     | deposit rejected: match has already completed        |
/// | 18   | NotPaused          | emergency_drain requires the contract to be paused   |
/// | 19   | InsufficientAllowance | player has not approved sufficient token allowance |
/// | 20   | DisputeWindowActive   | finalize_result called before the dispute window has expired |
/// | 21   | MatchTimedOut         | match has already timed out; use claim_timeout to reclaim funds |
/// | 22   | InvalidToken          | the provided token address does not match the initialized token |
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// [E001] No match exists for the given `match_id`.
    MatchNotFound = 1,

    /// [E002] The calling player has already deposited their stake for this match.
    AlreadyFunded = 2,

    /// [E003] `submit_result` was called before both players have deposited.
    NotFunded = 3,

    /// [E004] Caller is not the oracle, admin, or an authorised player for this operation.
    Unauthorized = 4,

    /// [E005] The requested operation is not valid in the match's current `MatchState`.
    /// Valid transitions: Pending → Active (deposit), Pending → Cancelled (cancel_match),
    /// Active → Completed (submit_result).
    InvalidState = 5,

    /// [E006] A match record already exists at this ID (internal counter collision).
    AlreadyExists = 6,

    /// [E007] `initialize` has already been called; the contract cannot be re-initialized.
    AlreadyInitialized = 7,

    /// [E008] The match ID counter has reached `u64::MAX` and cannot be incremented safely.
    Overflow = 8,

    /// [E009] The contract is paused by the admin. `create_match`, `deposit`, and
    /// `submit_result` are blocked until `unpause` is called.
    ContractPaused = 9,

    /// [E010] `stake_amount` must be a positive integer greater than zero.
    InvalidAmount = 10,

    /// [E011] `game_id` is empty or exceeds the 64-byte maximum length.
    InvalidGameId = 11,

    /// [E012] `player1` and `player2` must be different addresses; a player cannot
    /// bet against themselves.
    InvalidPlayers = 12,

    /// [E013] The oracle submitted a result whose `game_id` does not match the
    /// `game_id` stored in the match. Prevents cross-match result injection.
    GameIdMismatch = 13,

    /// [E014] The provided `game_id` is already linked to an existing match.
    /// Each game may only be used in one match.
    DuplicateGameId = 14,
    /// token transfer failed
    TransferFailed = 15,

    /// [E016] Deposit rejected because the match has been cancelled.
    MatchCancelled = 16,

    /// [E017] Deposit rejected because the match has already completed.
    MatchCompleted = 17,

    /// [E018] emergency_drain requires the contract to be paused first.
    NotPaused = 18,

    /// [E019] The player has not approved sufficient token allowance for the contract.
    /// Before calling `deposit`, the player must call `token.approve` to allow the
    /// escrow contract to transfer `stake_amount` tokens on their behalf.
    InsufficientAllowance = 19,

    /// [E020] `finalize_result` was called before the dispute window (`DISPUTE_WINDOW_LEDGERS`)
    /// has fully elapsed since the oracle submitted the result. Wait until the window expires.
    DisputeWindowActive = 20,

    /// [E021] The match has already been active for longer than `TIMEOUT_LEDGERS` without an
    /// oracle result. The match is effectively timed out; players should call `claim_timeout`.
    MatchTimedOut = 21,

    /// [E022] The provided token address does not match the token set during `initialize`.
    /// `create_match` requires the token to match the contract's configured token.
    InvalidToken = 22,
}
