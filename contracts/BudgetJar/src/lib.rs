#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol, Vec,
};

/// Storage keys for the contract
#[contracttype]
pub enum DataKey {
    /// Stores the admin address who initialized the contract
    Admin,
    /// Stores jar configuration for a user: (user_address) -> JarConfig
    JarConfig(Address),
    /// Stores balance for a specific jar: (user_address, jar_name) -> i128
    JarBalance(Address, Symbol),
    /// Stores the timestamp when savings jar was last deposited: (user_address) -> u64
    SavingsLockTime(Address),
}

/// Configuration for how deposits are split across jars
#[contracttype]
#[derive(Clone)]
pub struct JarConfig {
    /// Percentage allocated to "needs" jar (0-100)
    pub needs_pct: u32,
    /// Percentage allocated to "wants" jar (0-100)
    pub wants_pct: u32,
    /// Percentage allocated to "savings" jar (0-100)
    pub savings_pct: u32,
    /// USDC token contract address
    pub token: Address,
    /// Lock period for savings in seconds (default: 30 days = 2592000)
    pub savings_lock_seconds: u64,
    /// Penalty percentage for early savings withdrawal (0-100)
    pub early_withdrawal_penalty_pct: u32,
}

/// Jar identifiers as symbols
const JAR_NEEDS: Symbol = symbol_short!("needs");
const JAR_WANTS: Symbol = symbol_short!("wants");
const JAR_SAVINGS: Symbol = symbol_short!("savings");

#[contract]
pub struct BudgetJarContract;

#[contractimpl]
impl BudgetJarContract {
    /// Initializes the contract with an admin address
    /// Called once after deployment
    pub fn initialize(env: Env, admin: Address) {
        // Ensure not already initialized
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    /// Sets up jar configuration for a user
    /// Must be called before depositing
    /// 
    /// # Arguments
    /// * `user` - The user setting up their jars (must authorize)
    /// * `token` - USDC token contract address
    /// * `needs_pct` - Percentage for needs jar
    /// * `wants_pct` - Percentage for wants jar
    /// * `savings_pct` - Percentage for savings jar
    /// * `savings_lock_seconds` - How long savings are locked
    /// * `early_withdrawal_penalty_pct` - Penalty for early withdrawal
    pub fn setup_jars(
        env: Env,
        user: Address,
        token: Address,
        needs_pct: u32,
        wants_pct: u32,
        savings_pct: u32,
        savings_lock_seconds: u64,
        early_withdrawal_penalty_pct: u32,
    ) {
        // User must authorize this action
        user.require_auth();

        // Validate percentages sum to 100
        if needs_pct + wants_pct + savings_pct != 100 {
            panic!("percentages must sum to 100");
        }

        // Validate penalty is reasonable
        if early_withdrawal_penalty_pct > 50 {
            panic!("penalty cannot exceed 50%");
        }

        let config = JarConfig {
            needs_pct,
            wants_pct,
            savings_pct,
            token,
            savings_lock_seconds,
            early_withdrawal_penalty_pct,
        };

        env.storage()
            .persistent()
            .set(&DataKey::JarConfig(user.clone()), &config);

        // Initialize jar balances to zero
        env.storage()
            .persistent()
            .set(&DataKey::JarBalance(user.clone(), JAR_NEEDS), &0i128);
        env.storage()
            .persistent()
            .set(&DataKey::JarBalance(user.clone(), JAR_WANTS), &0i128);
        env.storage()
            .persistent()
            .set(&DataKey::JarBalance(user.clone(), JAR_SAVINGS), &0i128);
    }

    /// Deposits funds and auto-splits across jars according to user's config
    /// 
    /// # Arguments
    /// * `user` - The depositing user (must authorize)
    /// * `amount` - Total amount to deposit (in token smallest units)
    pub fn deposit(env: Env, user: Address, amount: i128) {
        user.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }

        // Get user's jar configuration
        let config: JarConfig = env
            .storage()
            .persistent()
            .get(&DataKey::JarConfig(user.clone()))
            .expect("jars not configured");

        // Calculate splits
        let needs_amount = (amount * config.needs_pct as i128) / 100;
        let wants_amount = (amount * config.wants_pct as i128) / 100;
        let savings_amount = amount - needs_amount - wants_amount; // Remainder to savings

        // Transfer tokens from user to contract
        let token_client = soroban_sdk::token::Client::new(&env, &config.token);
        token_client.transfer(&user, &env.current_contract_address(), &amount);

        // Update jar balances
        let needs_key = DataKey::JarBalance(user.clone(), JAR_NEEDS);
        let wants_key = DataKey::JarBalance(user.clone(), JAR_WANTS);
        let savings_key = DataKey::JarBalance(user.clone(), JAR_SAVINGS);

        let current_needs: i128 = env.storage().persistent().get(&needs_key).unwrap_or(0);
        let current_wants: i128 = env.storage().persistent().get(&wants_key).unwrap_or(0);
        let current_savings: i128 = env.storage().persistent().get(&savings_key).unwrap_or(0);

        env.storage()
            .persistent()
            .set(&needs_key, &(current_needs + needs_amount));
        env.storage()
            .persistent()
            .set(&wants_key, &(current_wants + wants_amount));
        env.storage()
            .persistent()
            .set(&savings_key, &(current_savings + savings_amount));

        // Update savings lock timestamp
        env.storage()
            .persistent()
            .set(&DataKey::SavingsLockTime(user), &env.ledger().timestamp());
    }

    /// Withdraws from needs or wants jar (no restrictions)
    /// 
    /// # Arguments
    /// * `user` - The withdrawing user (must authorize)
    /// * `jar` - Which jar to withdraw from ("needs" or "wants")
    /// * `amount` - Amount to withdraw
    pub fn withdraw(env: Env, user: Address, jar: Symbol, amount: i128) {
        user.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }

        // Cannot use this function for savings
        if jar == JAR_SAVINGS {
            panic!("use withdraw_savings for savings jar");
        }

        if jar != JAR_NEEDS && jar != JAR_WANTS {
            panic!("invalid jar name");
        }

        let config: JarConfig = env
            .storage()
            .persistent()
            .get(&DataKey::JarConfig(user.clone()))
            .expect("jars not configured");

        let balance_key = DataKey::JarBalance(user.clone(), jar);
        let current_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);

        if current_balance < amount {
            panic!("insufficient balance");
        }

        // Update balance
        env.storage()
            .persistent()
            .set(&balance_key, &(current_balance - amount));

        // Transfer tokens back to user
        let token_client = soroban_sdk::token::Client::new(&env, &config.token);
        token_client.transfer(&env.current_contract_address(), &user, &amount);
    }

    /// Withdraws from savings jar with lock period and penalty logic
    /// 
    /// # Arguments
    /// * `user` - The withdrawing user (must authorize)
    /// * `amount` - Amount to withdraw
    /// * `emergency` - If true, allows early withdrawal with penalty
    pub fn withdraw_savings(env: Env, user: Address, amount: i128, emergency: bool) {
        user.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }

        let config: JarConfig = env
            .storage()
            .persistent()
            .get(&DataKey::JarConfig(user.clone()))
            .expect("jars not configured");

        let balance_key = DataKey::JarBalance(user.clone(), JAR_SAVINGS);
        let current_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);

        if current_balance < amount {
            panic!("insufficient savings balance");
        }

        // Check lock period
        let lock_time: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::SavingsLockTime(user.clone()))
            .unwrap_or(0);

        let current_time = env.ledger().timestamp();
        let lock_expired = current_time >= lock_time + config.savings_lock_seconds;

        let payout_amount: i128;

        if lock_expired {
            // Lock period passed - full withdrawal
            payout_amount = amount;
        } else if emergency {
            // Emergency withdrawal with penalty
            let penalty = (amount * config.early_withdrawal_penalty_pct as i128) / 100;
            payout_amount = amount - penalty;
            // Penalty stays in contract (could be redistributed or burned)
        } else {
            panic!("savings locked until lock period expires");
        }

        // Update balance (full amount deducted regardless of penalty)
        env.storage()
            .persistent()
            .set(&balance_key, &(current_balance - amount));

        // Transfer payout to user
        let token_client = soroban_sdk::token::Client::new(&env, &config.token);
        token_client.transfer(&env.current_contract_address(), &user, &payout_amount);
    }

    /// Returns the balance of a specific jar for a user
    pub fn get_jar_balance(env: Env, user: Address, jar: Symbol) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::JarBalance(user, jar))
            .unwrap_or(0)
    }

    /// Returns all jar balances for a user
    pub fn get_all_balances(env: Env, user: Address) -> (i128, i128, i128) {
        let needs = env
            .storage()
            .persistent()
            .get(&DataKey::JarBalance(user.clone(), JAR_NEEDS))
            .unwrap_or(0);
        let wants = env
            .storage()
            .persistent()
            .get(&DataKey::JarBalance(user.clone(), JAR_WANTS))
            .unwrap_or(0);
        let savings = env
            .storage()
            .persistent()
            .get(&DataKey::JarBalance(user, JAR_SAVINGS))
            .unwrap_or(0);

        (needs, wants, savings)
    }

    /// Returns the jar configuration for a user
    pub fn get_config(env: Env, user: Address) -> JarConfig {
        env.storage()
            .persistent()
            .get(&DataKey::JarConfig(user))
            .expect("jars not configured")
    }

    /// Returns seconds remaining until savings unlock (0 if already unlocked)
    pub fn get_savings_lock_remaining(env: Env, user: Address) -> u64 {
        let config: JarConfig = env
            .storage()
            .persistent()
            .get(&DataKey::JarConfig(user.clone()))
            .expect("jars not configured");

        let lock_time: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::SavingsLockTime(user))
            .unwrap_or(0);

        let unlock_time = lock_time + config.savings_lock_seconds;
        let current_time = env.ledger().timestamp();

        if current_time >= unlock_time {
            0
        } else {
            unlock_time - current_time
        }
    }
}
