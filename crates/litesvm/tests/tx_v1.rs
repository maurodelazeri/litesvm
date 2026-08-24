//! Transaction v1 (SIMD-0385) behavior, pinned against agave's runtime rules:
//! the compute budget comes from the message's `TransactionConfig`, unset
//! limits resolve to zero, the priority fee is a total in lamports, the
//! loaded-accounts data-size limit is enforced during account loading, and the
//! whole format is refused while the `enable_tx_v1` feature gate is inactive.

use {
    litesvm::LiteSVM,
    solana_address::Address,
    solana_keypair::Keypair,
    solana_message::{v1, VersionedMessage},
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_system_interface::instruction::transfer,
    solana_transaction::versioned::VersionedTransaction,
    solana_transaction_error::TransactionError,
};

/// Base overhead agave's account loader charges per existing loaded account.
const TRANSACTION_ACCOUNT_BASE_SIZE: u32 = 64;

fn v1_enabled_svm() -> LiteSVM {
    let mut feature_set = LiteSVM::mainnet_feature_set();
    feature_set.activate(&agave_feature_set::enable_tx_v1::ID, 0);
    LiteSVM::new().with_feature_set(feature_set)
}

fn v1_transfer(
    svm: &LiteSVM,
    payer: &Keypair,
    recipient: &Address,
    lamports: u64,
    config: v1::TransactionConfig,
) -> VersionedTransaction {
    let instruction = transfer(&payer.pubkey(), recipient, lamports);
    let message = v1::Message::try_compile_with_config(
        &payer.pubkey(),
        &[instruction],
        svm.latest_blockhash(),
        config,
    )
    .unwrap();
    VersionedTransaction::try_new(VersionedMessage::V1(message), &[payer]).unwrap()
}

/// A config generous enough that only the behavior under test can fail.
fn working_config() -> v1::TransactionConfig {
    v1::TransactionConfig::empty()
        .with_compute_unit_limit(20_000)
        .with_loaded_accounts_data_size_limit(64 * 1024)
}

#[test_log::test]
fn v1_is_refused_until_the_feature_gate_activates() {
    // Default LiteSVM carries the mainnet feature snapshot, where
    // `enable_tx_v1` is not active.
    let mut svm = LiteSVM::new();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
    let tx = v1_transfer(&svm, &payer, &Address::new_unique(), 1_000, working_config());

    let err = svm.send_transaction(tx).unwrap_err();
    assert_eq!(err.err, TransactionError::UnsupportedVersion);
}

#[test_log::test]
fn v1_executes_and_charges_the_priority_fee_as_a_total() {
    let mut svm = v1_enabled_svm();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
    let recipient = Address::new_unique();

    // Two identical transfers, differing only in the 5_000-lamport priority
    // fee: the fee delta must be exactly that total (not a per-CU rate).
    let no_priority = svm
        .send_transaction(v1_transfer(&svm, &payer, &recipient, 1_000_000, working_config()))
        .unwrap();
    svm.expire_blockhash();
    let with_priority = svm
        .send_transaction(v1_transfer(
            &svm,
            &payer,
            &recipient,
            1_000_000,
            working_config().with_priority_fee(5_000),
        ))
        .unwrap();

    assert_eq!(with_priority.fee - no_priority.fee, 5_000);
    assert!(no_priority.compute_units_consumed > 0);
    assert!(no_priority.compute_units_consumed <= 20_000);
}

#[test_log::test]
fn v1_unset_compute_unit_limit_resolves_to_zero_and_fails() {
    let mut svm = v1_enabled_svm();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();

    let config = v1::TransactionConfig::empty().with_loaded_accounts_data_size_limit(64 * 1024);
    let err = svm
        .send_transaction(v1_transfer(&svm, &payer, &Address::new_unique(), 1_000_000, config))
        .unwrap_err();

    // Zero compute units: the first instruction immediately exhausts the budget.
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(
                0,
                solana_instruction::error::InstructionError::ComputationalBudgetExceeded
            )
        ),
        "unexpected error: {:?}",
        err.err
    );
}

#[test_log::test]
fn v1_unset_loaded_accounts_data_size_limit_resolves_to_zero_and_fails() {
    let mut svm = v1_enabled_svm();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();

    let config = v1::TransactionConfig::empty().with_compute_unit_limit(20_000);
    let err = svm
        .send_transaction(v1_transfer(&svm, &payer, &Address::new_unique(), 1_000_000, config))
        .unwrap_err();

    assert_eq!(err.err, TransactionError::MaxLoadedAccountsDataSizeExceeded);
}

#[test_log::test]
fn v1_loaded_accounts_data_size_limit_boundary_is_exact() {
    let mut svm = v1_enabled_svm();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
    let recipient_kp = Keypair::new();
    let recipient = recipient_kp.pubkey();
    // Fund the recipient so it exists and is charged base overhead too.
    svm.airdrop(&recipient, 1_000_000).unwrap();

    // Exactly what agave's loader counts for this transaction: base + data
    // for the payer, the recipient, and the system program account.
    let system_program_len = svm
        .get_account(&system_program::id())
        .unwrap()
        .data
        .len() as u32;
    let exact = TRANSACTION_ACCOUNT_BASE_SIZE * 3 + system_program_len;

    let err = svm
        .send_transaction(v1_transfer(
            &svm,
            &payer,
            &recipient,
            1_000_000,
            v1::TransactionConfig::empty()
                .with_compute_unit_limit(20_000)
                .with_loaded_accounts_data_size_limit(exact - 1),
        ))
        .unwrap_err();
    assert_eq!(err.err, TransactionError::MaxLoadedAccountsDataSizeExceeded);

    svm.send_transaction(v1_transfer(
        &svm,
        &payer,
        &recipient,
        1_000_000,
        v1::TransactionConfig::empty()
            .with_compute_unit_limit(20_000)
            .with_loaded_accounts_data_size_limit(exact),
    ))
    .unwrap();
}

#[test_log::test]
fn v1_heap_size_must_be_a_valid_frame() {
    let mut svm = v1_enabled_svm();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
    let recipient = Address::new_unique();

    // Not a multiple of 1024.
    let err = svm
        .send_transaction(v1_transfer(
            &svm,
            &payer,
            &recipient,
            1_000_000,
            working_config().with_heap_size(33 * 1024 + 512),
        ))
        .unwrap_err();
    assert_eq!(err.err, TransactionError::SanitizeFailure);

    // Below the 32 KiB minimum.
    let err = svm
        .send_transaction(v1_transfer(
            &svm,
            &payer,
            &recipient,
            1_000_000,
            working_config().with_heap_size(16 * 1024),
        ))
        .unwrap_err();
    assert_eq!(err.err, TransactionError::SanitizeFailure);

    // A valid 64 KiB frame works.
    svm.send_transaction(v1_transfer(
        &svm,
        &payer,
        &recipient,
        1_000_000,
        working_config().with_heap_size(64 * 1024),
    ))
    .unwrap();
}

#[test_log::test]
fn legacy_loaded_accounts_data_size_limit_is_enforced_too() {
    // The enforcement is not v1-specific: a legacy transaction that requests
    // a tiny limit via the ComputeBudget instruction must fail the same way.
    let mut svm = LiteSVM::new();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();

    let instructions = [
        solana_compute_budget_interface::ComputeBudgetInstruction::set_loaded_accounts_data_size_limit(100),
        transfer(&payer.pubkey(), &Address::new_unique(), 1_000_000),
    ];
    let message = solana_message::Message::new(&instructions, Some(&payer.pubkey()));
    let tx = solana_transaction::Transaction::new(&[&payer], message, svm.latest_blockhash());

    let err = svm.send_transaction(tx).unwrap_err();
    assert_eq!(err.err, TransactionError::MaxLoadedAccountsDataSizeExceeded);

    // And a plain legacy transfer (default 64 MiB limit) still works.
    let message = solana_message::Message::new(
        &[transfer(&payer.pubkey(), &Address::new_unique(), 1_000_000)],
        Some(&payer.pubkey()),
    );
    let tx = solana_transaction::Transaction::new(&[&payer], message, svm.latest_blockhash());
    svm.send_transaction(tx).unwrap();
}

#[test_log::test]
fn v1_simulation_uses_the_config_budget() {
    let mut svm = v1_enabled_svm();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();

    // Simulation shares the same budget seam: an unset loaded-size limit
    // fails there too, and a working config simulates fine.
    let bad = svm.simulate_transaction(v1_transfer(
        &svm,
        &payer,
        &Address::new_unique(),
        1_000_000,
        v1::TransactionConfig::empty().with_compute_unit_limit(20_000),
    ));
    assert_eq!(
        bad.unwrap_err().err,
        TransactionError::MaxLoadedAccountsDataSizeExceeded
    );

    svm.simulate_transaction(v1_transfer(
        &svm,
        &payer,
        &Address::new_unique(),
        1_000_000,
        working_config(),
    ))
    .unwrap();
}
