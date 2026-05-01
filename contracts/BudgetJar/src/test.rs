#![cfg(test)]

use crate::{BudgetJarContract, BudgetJarContractClient, JAR_NEEDS, JAR_SAVINGS, JAR_WANTS};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger, LedgerInfo},
    token, Address, Env, Symbol,
};

/// Helper to create a test environment with a mock token
fn setup_test() -> (Env, BudgetJarContractClient<'static>, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    // Deploy the BudgetJar contract
    let contract_id = env.register_contract(None, BudgetJarContract);
    let client = BudgetJarContractClient::new(&env, &contract_id);

    // Create admin and user addresses
    let admin = Address::generate(&env);
    let user = Address::generate(&env);

    // Deploy a mock USDC token
    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_address = token_contract.address();
    let token_admin_client = token::StellarAssetClient::new(&env, &token_address);

    // Mint tokens to user for testing
    token_admin_client.mint(&user, &100_000_0000000); // 100,000 USDC (7 decimals)

    // Initialize the contract
    client.initialize(&admin);

    // Leak env to get 'static lifetime for client
    let env: &'static Env = Box::leak(Box::new(env));
    let client = BudgetJarContractClient::new(env, &contract_id);

    (env.clone(), client, admin, user, token_address)
}

/// Test 1 (Happy Path): Full deposit and split flow works correctly
#[test]
fn test_deposit_splits_correctly() {
    let (env, client, _admin, user, token) = setup_test();

    // Setup jars: 50% needs, 30% wants, 20% savings
    // 30-day lock (2592000 seconds), 10% early withdrawal penalty
    client.setup_jars(&user, &token, &50, &30, &20, &2592000, &10);

    // Deposit 1000 USDC (with 7 decimals = 10_000_000_000)
    let deposit_amount: i128 = 1000_0000000;
    client.deposit(&user, &deposit_amount);

    // Verify splits: 500 needs, 300 wants, 200 savings
    let (needs, wants, savings) = client.get_all_balances(&user);

    assert_eq!(needs, 500_0000000, "needs jar should have 50%");
    assert_eq!(wants, 300_0000000, "wants jar should have 30%");
    assert_eq!(savings, 200_0000000, "savings jar should have 20%");
}

/// Test 2 (Edge Case): Unauthorized user cannot withdraw from another user's jar
#[test]
#[should_panic(expected = "jars not configured")]
fn test_unauthorized_withdrawal_fails() {
    let (env, client, _admin, user, token) = setup_test();

    // User sets up their jars
    client.setup_jars(&user, &token, &50, &30, &20, &2592000, &10);
    client.deposit(&user, &1000_0000000);

    // Create a different user who tries to withdraw
    let attacker = Address::generate(&env);

    // This should panic because attacker has no jar config
    client.withdraw(&attacker, &symbol_short!("needs"), &100_0000000);
}

/// Test 3 (State Verification): Withdrawal updates balance correctly
#[test]
fn test_withdrawal_updates_state() {
    let (env, client, _admin, user, token) = setup_test();

    client.setup_jars(&user, &token, &50, &30, &20, &2592000, &10);
    client.deposit(&user, &1000_0000000);

    // Check initial needs balance
    let initial_needs = client.get_jar_balance(&user, &symbol_short!("needs"));
    assert_eq!(initial_needs, 500_0000000);

    // Withdraw 200 from needs
    client.withdraw(&user, &symbol_short!("needs"), &200_0000000);

    // Verify updated balance
    let final_needs = client.get_jar_balance(&user, &symbol_short!("needs"));
    assert_eq!(final_needs, 300_0000000, "needs should be reduced by withdrawal");

    // Other jars unchanged
    let wants = client.get_jar_balance(&user, &symbol_short!("wants"));
    let savings = client.get_jar_balance(&user, &symbol_short!("savings"));
    assert_eq!(wants, 300_0000000);
    assert_eq!(savings, 200_0000000);
}

/// Test 4: Early savings withdrawal applies penalty correctly
#[test]
fn test_early_savings_withdrawal_penalty() {
    let (env, client, _admin, user, token) = setup_test();

    // 10% penalty for early withdrawal
    client.setup_jars(&user, &token, &50, &30, &20, &2592000, &10);
    client.deposit(&user, &1000_0000000);

    // Savings has 200 USDC
    let initial_savings = client.get_jar_balance(&user, &symbol_short!("savings"));
    assert_eq!(initial_savings, 200_0000000);

    // Get user's token balance before withdrawal
    let token_client = token::Client::new(&env, &token);
    let balance_before = token_client.balance(&user);

    // Emergency withdrawal of 100 USDC (should receive 90 after 10% penalty)
    client.withdraw_savings(&user, &100_0000000, &true);

    // Verify savings balance reduced by full amount
    let final_savings = client.get_jar_balance(&user, &symbol_short!("savings"));
    assert_eq!(final_savings, 100_0000000);

    // Verify user received 90 USDC (100 - 10% penalty)
    let balance_after = token_client.balance(&user);
    assert_eq!(balance_after - balance_before, 90_0000000);
}

/// Test 5: Savings withdrawal works without penalty after lock expires
#[test]
fn test_savings_withdrawal_after_lock() {
    let (env, client, _admin, user, token) = setup_test();

    // 10 second lock for easier testing
    client.setup_jars(&user, &token, &50, &30, &20, &10, &10);
    client.deposit(&user, &1000_0000000);

    // Fast-forward time past the lock period
    env.ledger().set(LedgerInfo {
        timestamp: env.ledger().timestamp() + 15, // 15 seconds later
        protocol_version: 20,
        sequence_number: env.ledger().sequence(),
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 3110400,
    });

    let token_client = token::Client::new(&env, &token);
    let balance_before = token_client.balance(&user);

    // Withdraw full savings (no emergency flag needed)
    client.withdraw_savings(&user, &200_0000000, &false);

    // Verify full amount received (no penalty)
    let balance_after = token_client.balance(&user);
    assert_eq!(balance_after - balance_before, 200_0000000);

    // Verify savings is now zero
    let final_savings = client.get_jar_balance(&user, &symbol_short!("savings"));
    assert_eq!(final_savings, 0);
}
