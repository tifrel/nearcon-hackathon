use near_sdk::{
    borsh::{
        self,
        BorshDeserialize,
        BorshSerialize,
    },
    collections::UnorderedMap,
    env,
    json_types::U128,
    serde::{
        Deserialize,
        Serialize,
    },
    AccountId,
    Balance,
    Gas,
    PanicOnDefault,
    Promise,
    PromiseError,
    PromiseOrValue,
};

// ------------------------------- constants -------------------------------- //
// One milliNEAR -> TODO: calculate proper amount
const REGISTRATION_DEPOSIT: Balance = 1000_000000_000000_000000;
// One milliNEAR -> TODO: calculate proper amount
const MOTION_DEPOSIT: Balance = 1000_000000_000000_000000;

const NFT_TRANSFER_GAS: Gas = Gas(100_000_000_000_000);
const RESOLVE_SALE_GAS: Gas = Gas(100_000_000_000_000);

// -------------------------- external interfaces --------------------------- //
#[near_sdk::ext_contract(ext_nft)]
trait ExtNft {
    fn nft_transfer(&self, receiver_id: AccountId, token_id: String);
}

#[near_sdk::ext_contract(ext_fungifier)]
trait ExtFungifier {
    fn resolve_sale(&self);
}

// ---------------------------- contract storage ---------------------------- //
#[near_sdk::near_bindgen]
#[derive(BorshSerialize, BorshDeserialize, PanicOnDefault)]
pub struct Fungifier {
    deployer_id: AccountId,
    nft_contract_id: AccountId,
    nft_token_id: String,
    total_supply: Balance,
    ft_owners: UnorderedMap<AccountId, Balance>,
    dao_participation_threshold: Balance,
    dao_acceptance_threshold: Balance, // should probably be a ratio
    motions: UnorderedMap<String, Motion>,
    // sale_motions: UnorderedMap<String, SaleMotion>,
    // misc_motions: UnorderedMap<String, MiscMotion>,
    cashout_amount: Option<Balance>,
    sale_in_progress_id: Option<String>,
}

// this enum makes development harder, but allows upgrading more custom motions
#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub enum Motion {
    Sale(SaleMotion),
    // TODO: maintenance motion
    Misc(MiscMotion),
}

impl Motion {
    fn unwrap_sale(&self) -> &SaleMotion {
        match self {
            Self::Sale(motion) => &motion,
            _ => env::panic_str("Motion is not a Sale!"),
        }
    }
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone)]
#[serde(crate = "near_sdk::serde")]
pub struct SaleMotion {
    receiver_id: AccountId,
    sale_price: Balance,
    votes: Votes,
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone)]
#[serde(crate = "near_sdk::serde")]
pub struct MiscMotion {
    initiator_id: AccountId,
    description: String,
    votes: Votes,
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone)]
#[serde(crate = "near_sdk::serde")]
pub struct Votes {
    pub accepting: Vec<AccountId>,
    pub rejecting: Vec<AccountId>,
    pub indifferent: Vec<AccountId>,
}

impl Votes {
    pub fn new() -> Self {
        Votes {
            accepting: Vec::new(),
            rejecting: Vec::new(),
            indifferent: Vec::new(),
        }
    }

    pub fn total_votes(
        &self,
        balances: &UnorderedMap<AccountId, Balance>,
    ) -> Balance {
        let mut total = 0;
        for account_id in self.accepting.iter() {
            total += balances.get(account_id).unwrap_or(0);
        }
        for account_id in self.rejecting.iter() {
            total += balances.get(account_id).unwrap_or(0);
        }
        for account_id in self.indifferent.iter() {
            total += balances.get(account_id).unwrap_or(0);
        }
        total
    }

    pub fn favorable_votes(
        &self,
        balances: &UnorderedMap<AccountId, Balance>,
    ) -> Balance {
        let mut total = 0;
        for account_id in self.accepting.iter() {
            total += balances.get(account_id).unwrap_or(0);
        }
        total
    }
}

// ----------------------------- contract logic ----------------------------- //
#[near_sdk::near_bindgen]
impl Fungifier {
    #[init]
    pub fn init(
        deployer_id: AccountId,
        nft_contract_id: AccountId,
        nft_token_id: String,
        total_supply: U128,
        dao_participation_threshold: U128, //
        dao_acceptance_threshold: U128,
    ) -> Self {
        Self {
            deployer_id,
            nft_contract_id,
            nft_token_id,
            total_supply: total_supply.into(),
            ft_owners: UnorderedMap::new(b"o"),
            dao_participation_threshold: dao_participation_threshold.0,
            dao_acceptance_threshold: dao_acceptance_threshold.0,
            motions: UnorderedMap::new(b"m"),
            cashout_amount: None,
            sale_in_progress_id: None,
        }
    }

    // owner registration to prevent "million cheap data additions" attack
    #[payable]
    pub fn register(&mut self) {
        near_sdk::require!(
            env::attached_deposit() == REGISTRATION_DEPOSIT,
            "Registration deposit needs to be exactly one milliNEAR"
        );
        self.ft_owners.insert(&env::predecessor_account_id(), &0);
    }

    #[payable]
    pub fn init_sell_motion(&mut self, sale_price: U128, motion_id: String) {
        near_sdk::require!(
            env::attached_deposit() == MOTION_DEPOSIT,
            "Motion deposit needs to be exactly one milliNEAR"
        );
        if self.motions.get(&motion_id).is_some() {
            env::panic_str("Motion ID is already in use")
        }

        self.motions.insert(
            &motion_id,
            &Motion::Sale(SaleMotion {
                receiver_id: env::predecessor_account_id(),
                sale_price: sale_price.0,
                votes: Votes::new(),
            }),
        );
    }

    fn get_sale_motion_panicking(&self, motion_id: &String) -> SaleMotion {
        match self.motions.get(motion_id) {
            None => env::panic_str("No motion with that ID"),
            Some(Motion::Sale(motion)) => motion,
            Some(_) => env::panic_str("Motion does not refer to sale"),
        }
    }

    #[payable]
    pub fn finish_sale_motion(
        &mut self,
        motion_id: String,
    ) -> PromiseOrValue<bool> {
        let tx_sender = env::predecessor_account_id();
        // find motion
        let motion = self.get_sale_motion_panicking(&motion_id);

        // check only receiver or initiator can finalize the motion
        if &motion.receiver_id != &tx_sender {
            env::panic_str("Sale motion may only be ended by receiver");
        }

        // check deposit is sufficient
        if env::attached_deposit() < motion.sale_price + 1 {
            env::panic_str("Deposit is insufficient");
        }

        // check that no other sale is currently being processed
        if self.sale_in_progress_id.is_some() {
            env::panic_str("Another sale is currently in progress");
        }

        // check that the contract is still active
        if self.cashout_amount.is_some() {
            env::panic_str(
                "The NFT associated to this account has already been sold",
            );
        }

        // check thresholds
        let participated = motion.votes.total_votes(&self.ft_owners);
        let accepted = motion.votes.favorable_votes(&self.ft_owners);

        if participated < self.dao_participation_threshold {
            env::log_str(
                "Participation threshold not reached, motion rejected",
            );
            return PromiseOrValue::Value(false);
        }

        if accepted < self.dao_acceptance_threshold {
            env::log_str("Acceptance threshold not reached, motion rejected");
            return PromiseOrValue::Value(false);
        }

        // mark contract as completed
        self.cashout_amount = Some(env::attached_deposit());
        self.sale_in_progress_id = Some(motion_id);

        // transfer the NFT
        let nft_transfer_promise = ext_nft::ext(self.nft_contract_id.clone())
            .with_attached_deposit(1)
            .with_static_gas(NFT_TRANSFER_GAS)
            .nft_transfer(
                motion.receiver_id.clone(),
                self.nft_token_id.clone(),
            );

        // callback promise
        let callback_promise = ext_fungifier::ext(env::current_account_id())
            .with_static_gas(RESOLVE_SALE_GAS)
            .resolve_sale();

        return PromiseOrValue::Promise(
            nft_transfer_promise.then(callback_promise),
        );
    }

    #[private]
    pub fn resolve_sale(
        &mut self,
        #[callback_result] call_result: Result<(), PromiseError>,
    ) -> bool {
        // Motion existance was verified, unwrap ok!
        let motion_id = self.sale_in_progress_id.clone().unwrap();
        let motion =
            self.motions.get(&motion_id).unwrap().unwrap_sale().clone();
        let sale_price = self.cashout_amount.unwrap();

        if call_result.is_err() {
            // transfer failed -> refund sale price, unlock contract, return false
            env::log_str("NFT transfer failed");
            Promise::new(motion.receiver_id.clone()).transfer(sale_price);
            self.sale_in_progress_id = None;
            self.cashout_amount = None;
            return false;
        } else {
            // transfer successful -> refund deposit, return true
            self.sale_in_progress_id = None;
            Promise::new(motion.receiver_id.clone()).transfer(MOTION_DEPOSIT);
            return true;
        }
    }

    // withdraw motion
    pub fn withdraw_sale_motion(&mut self, motion_id: String) {
        let motion = self.get_sale_motion_panicking(&motion_id);
        if env::predecessor_account_id() != motion.receiver_id {
            env::panic_str("Only the motion receiver can withdraw it");
        }

        self.motions.remove(&motion_id);
        Promise::new(motion.receiver_id).transfer(MOTION_DEPOSIT);
    }

    // cashout function once the cashout has been accomplished
    pub fn cashout(&mut self) {
        near_sdk::assert_one_yocto();

        if let Some(cashout_amount) = self.cashout_amount {
            let receiver_id = env::predecessor_account_id();
            let share = self.ft_owners.get(&receiver_id).unwrap_or(0)
                * 1_000_000
                / self.total_supply;
            let cashout = cashout_amount * share / 1_000_000;

            Promise::new(receiver_id.clone()).transfer(cashout);
            self.ft_owners.insert(&receiver_id, &0);
        } else {
            env::panic_str("Cannot cash out of an unsold NFT!");
        }
    }

    // TODO: undeploy function to recover storage deposit
}

// NEP141
#[near_sdk::near_bindgen]
impl Fungifier {
    fn get_balance_internal(&self, account_id: &AccountId) -> u128 {
        match self.ft_owners.get(account_id) {
            None => {
                env::panic_str(&format!("{} is not registered.", account_id))
            }
            Some(balance) => balance,
        }
    }

    fn ft_transfer_internal(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: Balance,
    ) {
        let sender_balance = self.get_balance_internal(&sender_id);
        let receiver_balance = self.get_balance_internal(&receiver_id);
        near_sdk::require!(
            sender_balance > amount,
            "Sender does not own sufficient shares!"
        );

        self.ft_owners
            .insert(&sender_id, &(sender_balance - amount));
        self.ft_owners
            .insert(&receiver_id, &(receiver_balance + amount));

        // FIXME: emit event!
    }

    #[payable]
    pub fn ft_transfer(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        #[allow(unused)] memo: Option<String>,
    ) {
        near_sdk::assert_one_yocto();
        self.ft_transfer_internal(
            env::predecessor_account_id(),
            receiver_id,
            amount.0,
        );
    }

    // TODO: ft_transfer_call + resolve

    pub fn ft_total_supply(&self) -> U128 {
        self.total_supply.into()
    }

    pub fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.get_balance_internal(&account_id).into()
    }
}
