use crate::program::AquaDex;
use std::{ io::Cursor, string::String, result::Result as FnResult, mem::size_of, convert::TryFrom };
use bytemuck::{ Pod, Zeroable, cast_slice_mut, cast_slice };
use num_enum::{ TryFromPrimitive, IntoPrimitive };
use arrayref::{ mut_array_refs, array_refs };
use anchor_lang::prelude::*;
use anchor_spl::token::{ self, Token, Transfer as SPL_Transfer, TokenAccount as SPL_TokenAccount };
use anchor_spl::associated_token::{ self, AssociatedToken };
use solana_program::{
    sysvar, system_program,
    program::{ invoke }, clock::Clock,
    account_info::AccountInfo,
    instruction::{ AccountMeta, Instruction }
};

extern crate slab_alloc;
use slab_alloc::{ SlabPageAlloc, CritMapHeader, CritMap, AnyNode, LeafNode, SlabVec, SlabTreeError };

extern crate security_token;
use security_token::{ cpi::accounts::{ Transfer as AST_Transfer, CreateAccount as AST_CreateAccount } };

declare_id!("AQUAvuZCFUGtSc8uQBaTXfJz3YjMUbinMeXDoDQmZLvX");

pub const VERSION_MAJOR: u32 = 1;
pub const VERSION_MINOR: u32 = 0;
pub const VERSION_PATCH: u32 = 0;

// TESTING
pub const MAX_ORDERS: u32 = 500;        // Max orders on each side of the orderbook (16K * 7)
pub const MAX_TRADES: u32 = 100;        // Max trade entries in the trade log
pub const MAX_ACCOUNTS: u32 = 1500;     // Max number of accounts per settlement data file (16K * 8)
pub const MAX_EVICTIONS: u32 = 10;      // Max number of orders to evict before aborting
pub const MAX_EXPIRATIONS: u32 = 10;    // Max number of expired orders to remove before proceeding with current order

#[repr(u8)]
#[derive(PartialEq, Debug, Eq, Copy, Clone, TryFromPrimitive, IntoPrimitive)]
pub enum Side {
    Bid = 0,
    Ask = 1,
}

#[repr(u8)]
#[derive(PartialEq, Debug, Eq, Copy, Clone, TryFromPrimitive, IntoPrimitive)]
pub enum MintType {
    SPLToken = 0,
    AtxSecurityToken = 1,
}

#[repr(u16)]
#[derive(PartialEq, Debug, Eq, Copy, Clone)]
pub enum DT { // All data types
    BidOrderMap,
    BidOrder,
    AskOrderMap,
    AskOrder,
    AccountMap,
    Account,
}

#[repr(u16)]
#[derive(PartialEq, Debug, Eq, Copy, Clone, IntoPrimitive)]
pub enum OrderDT {          // Orders data types
    BidOrderMap,            // CritMap - bid side of the orderbook
    AskOrderMap,            // CritMap - ask side of the orderbook
    BidOrder,               // SlabVec - bid order details
    AskOrder,               // SlabVec - ask order details
}

#[repr(u16)]
#[derive(PartialEq, Debug, Eq, Copy, Clone, IntoPrimitive)]
pub enum SettleDT {         // Account settlement data types
    AccountMap,             // CritMap - settled account balances (cleared trades and evicted orders)
    Account,                // SlabVec - details of settled transactions
}

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct Order {
    pub amount: u64,
    pub expiry: i64,
}
unsafe impl Zeroable for Order {}
unsafe impl Pod for Order {}

impl Order {
    pub fn amount(&self) -> u64 {
        self.amount
    }

    pub fn set_amount(&mut self, new_amount: u64) {
        self.amount = new_amount
    }

    // Critbit Tree key functions
    pub fn new_key(state: &mut MarketState, side: Side, price: u64) -> u128 {
        let seq = state.order_counter;
        state.order_counter = state.order_counter + 1;
        let upper = (price as u128) << 64;
        let lower = match side {
            Side::Bid => !seq,
            Side::Ask => seq,
        };
        upper | (lower as u128)
    }

    pub fn price(key: u128) -> u64 {
        (key >> 64) as u64
    }

    pub fn next_index(pt: &mut SlabPageAlloc, data_type: DT) -> FnResult<u32, ProgramError> {
        let svec = pt.header_mut::<SlabVec>(index_datatype(data_type));
        let free_top = svec.free_top();
        if free_top == 0 { // Empty free list
            return Ok(svec.next_index());
        }
        let free_index = free_top.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
        let index_act = pt.index::<Order>(index_datatype(data_type), free_index as usize);
        let index_ptr = u32::try_from(index_act.amount()).expect("Invalid index");
        pt.header_mut::<SlabVec>(index_datatype(data_type)).set_free_top(index_ptr);
        Ok(free_index)
    }

    pub fn free_index(pt: &mut SlabPageAlloc, data_type: DT, idx: u32) -> anchor_lang::Result<()> {
        let free_top = pt.header::<SlabVec>(index_datatype(data_type)).free_top();
        pt.index_mut::<Order>(index_datatype(data_type), idx as usize).set_amount(free_top as u64);
        let new_top = idx.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
        pt.header_mut::<SlabVec>(index_datatype(data_type)).set_free_top(new_top);
        Ok(())
    }
}

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct AccountsHeader {
    pub market: Pubkey,     // Market address
    pub prev: Pubkey,       // Prev settlement accounts file
    pub next: Pubkey,       // Next settlement accounts file
    pub items: u32,         // Number of items in the log
}
unsafe impl Zeroable for AccountsHeader {}
unsafe impl Pod for AccountsHeader {}

impl AccountsHeader {
    pub fn set_prev(&mut self, key: &Pubkey) {
        self.prev = *key
    }

    pub fn set_next(&mut self, key: &Pubkey) {
        self.next = *key
    }
}

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct AccountEntry {
    pub mkt_token_balance: u64,
    pub prc_token_balance: u64,
    pub ts_updated: i64,
}
unsafe impl Zeroable for AccountEntry {}
unsafe impl Pod for AccountEntry {}

impl AccountEntry {
    pub fn mkt_token_balance(&self) -> u64 {
        self.mkt_token_balance
    }

    pub fn prc_token_balance(&self) -> u64 {
        self.prc_token_balance
    }

    pub fn ts_updated(&self) -> i64 {
        self.ts_updated
    }

    pub fn set_mkt_token_balance(&mut self, bal: u64) {
        self.mkt_token_balance = bal;
    }

    pub fn set_prc_token_balance(&mut self, bal: u64) {
        self.prc_token_balance = bal;
    }

    pub fn set_ts_updated(&mut self, ts: i64) {
        self.ts_updated = ts;
    }

    fn next_index(pt: &mut SlabPageAlloc, data_type: DT) -> FnResult<u32, ProgramError> {
        let svec = pt.header_mut::<SlabVec>(index_datatype(data_type));
        let free_top = svec.free_top();
        if free_top == 0 { // Empty free list
            return Ok(svec.next_index());
        }
        let free_index = free_top.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
        let index_act = pt.index::<AccountEntry>(index_datatype(data_type), free_index as usize);
        let index_ptr = u32::try_from(index_act.mkt_token_balance()).expect("Invalid index");
        pt.header_mut::<SlabVec>(index_datatype(data_type)).set_free_top(index_ptr);
        Ok(free_index)
    }

    fn free_index(pt: &mut SlabPageAlloc, data_type: DT, idx: u32) -> anchor_lang::Result<()> {
        let free_top = pt.header::<SlabVec>(index_datatype(data_type)).free_top();
        pt.index_mut::<AccountEntry>(index_datatype(data_type), idx as usize).set_mkt_token_balance(free_top as u64);
        let new_top = idx.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
        pt.header_mut::<SlabVec>(index_datatype(data_type)).set_free_top(new_top);
        Ok(())
    }
}

#[derive(Copy, Clone, Default)]
#[repr(packed)]
pub struct TradeLogHeader {
    pub trade_count: u64,
    pub entry_max: u64,
}
unsafe impl Zeroable for TradeLogHeader {}
unsafe impl Pod for TradeLogHeader {}

#[derive(Copy, Clone, Default)]
#[repr(packed)]
pub struct TradeEntry {
    pub event_type: u128,
    pub action_id: u64,
    pub trade_id: u64,
    pub maker_order_id: u128,
    pub maker_filled: bool,
    pub maker: Pubkey,
    pub taker: Pubkey,
    pub taker_side: u8,
    pub amount: u64,
    pub price: u64,
    pub ts: i64,
}
unsafe impl Zeroable for TradeEntry {}
unsafe impl Pod for TradeEntry {}

#[inline]
fn map_datatype(data_type: DT) -> u16 {
    match data_type {
        DT::BidOrder => OrderDT::BidOrderMap as u16,
        DT::AskOrder => OrderDT::AskOrderMap as u16,
        DT::Account  => SettleDT::AccountMap as u16,
        _ => { panic!("Invalid datatype") },
    }
}

#[inline]
fn map_len(data_type: DT) -> u32 {
    match data_type {
        DT::BidOrder => MAX_ORDERS,
        DT::AskOrder => MAX_ORDERS,
        DT::Account  => MAX_ACCOUNTS,
        _ => { panic!("Invalid datatype") },
    }
}

#[inline]
fn index_datatype(data_type: DT) -> u16 {
    match data_type {
        DT::BidOrder => OrderDT::BidOrder as u16,
        DT::AskOrder => OrderDT::AskOrder as u16,
        DT::Account => SettleDT::Account as u16,
        _ => { panic!("Invalid datatype") },
    }
}

#[inline]
fn map_get(pt: &mut SlabPageAlloc, data_type: DT, key: u128) -> Option<LeafNode> {
    let cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.get_key(key);
    match res {
        None => None,
        Some(res) => Some(res.clone()),
    }
}

#[inline]
fn map_min(pt: &mut SlabPageAlloc, data_type: DT) -> Option<LeafNode> {
    let cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.get_min();
    match res {
        None => None,
        Some(res) => Some(res.clone()),
    }
}

#[inline]
fn map_max(pt: &mut SlabPageAlloc, data_type: DT) -> Option<LeafNode> {
    let cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.get_max();
    match res {
        None => None,
        Some(res) => Some(res.clone()),
    }
}

#[inline]
fn map_predicate_min<F: FnMut(&SlabPageAlloc, &LeafNode) -> bool>(pt: &mut SlabPageAlloc, data_type: DT, predicate: F) -> Option<LeafNode> {
    let cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.predicate_min(predicate);
    match res {
        None => None,
        Some(res) => Some(res.clone()),
    }
}

#[inline]
fn map_predicate_max<F: FnMut(&SlabPageAlloc, &LeafNode) -> bool>(pt: &mut SlabPageAlloc, data_type: DT, predicate: F) -> Option<LeafNode> {
    let cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.predicate_max(predicate);
    match res {
        None => None,
        Some(res) => Some(res.clone()),
    }
}

#[inline]
fn map_insert(pt: &mut SlabPageAlloc, data_type: DT, node: &LeafNode) -> FnResult<(), SlabTreeError> {
    let mut cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.insert_leaf(node);
    match res {
        Err(SlabTreeError::OutOfSpace) => {
            //msg!("Atellix: Out of space...");
            return Err(SlabTreeError::OutOfSpace)
        },
        _  => Ok(())
    }
}

#[inline]
fn map_remove(pt: &mut SlabPageAlloc, data_type: DT, key: u128) -> anchor_lang::Result<()> {
    let mut cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    cm.remove_by_key(key).ok_or(error!(ErrorCode::RecordNotFound))?;
    Ok(())
}

#[inline]
fn load_struct<T: AccountDeserialize>(acc: &AccountInfo) -> FnResult<T, ProgramError> {
    let mut data: &[u8] = &acc.try_borrow_data()?;
    Ok(T::try_deserialize(&mut data)?)
}

#[inline]
fn store_struct<T: AccountSerialize>(obj: &T, acc: &AccountInfo) -> FnResult<(), Error> {
    let mut data = acc.try_borrow_mut_data()?;
    let dst: &mut [u8] = &mut data;
    let mut crs = Cursor::new(dst);
    obj.try_serialize(&mut crs)
}

#[inline]
fn scale_price(quantity: u64, price: u64, decimal_factor: u64) -> anchor_lang::Result<u64> {
    let mut tokens_calc: u128 = (quantity as u128).checked_mul(price as u128).ok_or(error!(ErrorCode::Overflow))?;
    tokens_calc = tokens_calc.checked_div(decimal_factor as u128).ok_or(error!(ErrorCode::Overflow))?;
    let tokens: u64 = u64::try_from(tokens_calc).map_err(|_| error!(ErrorCode::Overflow))?;
    return Ok(tokens);
}

#[inline]
fn fill_quantity(input_price: u64, order_price: u64, decimal_factor: u64) -> anchor_lang::Result<u64> {
    let mut tokens_calc: u128 = (input_price as u128).checked_mul(decimal_factor as u128).ok_or(error!(ErrorCode::Overflow))?;
    tokens_calc = tokens_calc.checked_div(order_price as u128).ok_or(error!(ErrorCode::Overflow))?;
    let tokens: u64 = u64::try_from(tokens_calc).map_err(|_| error!(ErrorCode::Overflow))?;
    return Ok(tokens);
}

#[inline]
fn calculate_fee(fee_rate: u32, base_amount: u64) -> anchor_lang::Result<u64> {
    let mut fee: u128 = (base_amount as u128).checked_mul(fee_rate as u128).ok_or(error!(ErrorCode::Overflow))?;
    fee = fee.checked_div(10000000).ok_or(error!(ErrorCode::Overflow))?;
    let result = u64::try_from(fee).map_err(|_| error!(ErrorCode::Overflow))?;
    Ok(result)
}

fn verify_matching_accounts(left: &Pubkey, right: &Pubkey, error_msg: Option<String>) -> anchor_lang::Result<()> {
    if *left != *right {
        if error_msg.is_some() {
            msg!(error_msg.unwrap().as_str());
            msg!("Expected: {}", left.to_string());
            msg!("Received: {}", right.to_string());
        }
        return Err(ErrorCode::InvalidAccount.into());
    }
    Ok(())
}

fn settle_account(settle: &AccountInfo, owner_id: u128, owner: &Pubkey, mkt_token: bool, amount: u64) -> FnResult<u64, Error> {
    let clock = Clock::get()?;
    let clock_ts = clock.unix_timestamp;
    let new_balance: u64;
    let log_data: &mut[u8] = &mut settle.try_borrow_mut_data()?;
    let (header, page_table) = mut_array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
    let settle_header: &mut [AccountsHeader] = cast_slice_mut(header);
    let sl = SlabPageAlloc::new(page_table);
    let has_item = map_get(sl, DT::Account, owner_id);
    if has_item.is_none() {
        new_balance = amount;
        let new_item = map_insert(sl, DT::Account, &LeafNode::new(owner_id, 0, owner));
        if new_item.is_ok() {
            // Delay setting the slot parameter so that AccountEntry SlabVec index is not updated unless a key is actually added to the CritMap
            let acct_idx = AccountEntry::next_index(sl, DT::Account)?;
            let mut cm = CritMap { slab: sl, type_id: map_datatype(DT::Account), capacity: map_len(DT::Account) };
            cm.get_key_mut(owner_id).unwrap().set_slot(acct_idx);
            let mut mkt_bal: u64 = 0;
            let mut prc_bal: u64 = 0;
            if mkt_token {
                mkt_bal = amount;
            } else {
                prc_bal = amount;
            }
            let acct = AccountEntry {
                mkt_token_balance: mkt_bal,
                prc_token_balance: prc_bal,
                ts_updated: clock_ts,
            };
            *sl.index_mut::<AccountEntry>(SettleDT::Account.into(), acct_idx as usize) = acct;
            settle_header[0].items = settle_header[0].items.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
        } else {
            return Err(error!(ErrorCode::SettlementLogFull));
        }
    } else {
        let log_item = has_item.unwrap();
        let current_acct = sl.index::<AccountEntry>(SettleDT::Account.into(), log_item.slot() as usize);
        let mut mkt_bal: u64 = current_acct.mkt_token_balance;
        let mut prc_bal: u64 = current_acct.prc_token_balance;
        let entry = sl.index_mut::<AccountEntry>(SettleDT::Account.into(), log_item.slot() as usize);
        if mkt_token {
            mkt_bal = mkt_bal.checked_add(amount).ok_or(error!(ErrorCode::Overflow))?;
            entry.set_mkt_token_balance(mkt_bal);
            new_balance = mkt_bal;
        } else {
            prc_bal = prc_bal.checked_add(amount).ok_or(error!(ErrorCode::Overflow))?;
            entry.set_prc_token_balance(prc_bal);
            new_balance = prc_bal;
        }
    }
    Ok(new_balance)
}

fn log_settlement(
    market_key: &Pubkey, 
    state: &mut MarketState, 
    settle_a: &AccountInfo,
    settle_b: &AccountInfo,
    owner: &Pubkey,
    mkt_token: bool,
    amount: u64,
) -> anchor_lang::Result<()> {
    //msg!("Atellix: Log Settlement");
    let new_balance: u64;
    let mut log_key: Pubkey = settle_a.key();
    let owner_id: u128 = CritMap::bytes_hash(owner.as_ref());
    let res = settle_account(settle_a, owner_id, owner, mkt_token, amount);
    if res.is_err() {
        let err = res.unwrap_err();
        if err == error!(ErrorCode::SettlementLogFull) {
            state.log_rollover = true;
            log_key = settle_b.key();
            let res2 = settle_account(settle_b, owner_id, owner, mkt_token, amount);
            if res2.is_err() {
                let err2 = res2.unwrap_err();
                if err2 == error!(ErrorCode::SettlementLogFull) {
                    msg!("Both settlement logs are full");
                }
                return Err(err2);
            } else {
                new_balance = res2.unwrap();
            }
        } else {
            return Err(err);
        }
    } else {
        new_balance = res.unwrap();
    }

    if mkt_token {
        msg!("Atellix: Settle Market Token - Amt: {} Bal: {} Key: {}", amount.to_string(), new_balance.to_string(), owner.to_string());
    } else {
        msg!("Atellix: Settle Pricing Token - Amt: {} Bal: {} Key: {}", amount.to_string(), new_balance.to_string(), owner.to_string());
    }

    if mkt_token {
        state.mkt_order_balance = state.mkt_order_balance.checked_sub(amount).ok_or(error!(ErrorCode::Overflow))?;
        state.mkt_log_balance = state.mkt_log_balance.checked_add(amount).ok_or(error!(ErrorCode::Overflow))?;
        /*msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
            state.mkt_vault_balance.to_string(),
            state.mkt_order_balance.to_string(),
        );*/
    } else {
        state.prc_order_balance = state.prc_order_balance.checked_sub(amount).ok_or(error!(ErrorCode::Overflow))?;
        state.prc_log_balance = state.prc_log_balance.checked_add(amount).ok_or(error!(ErrorCode::Overflow))?;
        /*msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
            state.prc_vault_balance.to_string(),
            state.prc_order_balance.to_string(),
        );*/
    }

    msg!("atellix-log");
    emit!(SettleEvent {
        event_type: 33111472894808803319726137140961827977, // solana/program/aqua-dex/settle_event
        action_id: state.action_counter,
        market: *market_key,
        owner: *owner,
        settlement_log: log_key,
        market_tokens: if mkt_token { amount } else { 0 },
        pricing_tokens: if !mkt_token { amount } else { 0 },
    });

    Ok(())
}

fn log_rollover(
    market_state: &mut MarketState,
    market_key: Pubkey,
    settle_b: &AccountInfo,
    settle_n: &AccountInfo, // New log account
) -> anchor_lang::Result<()> {

    // Add new log entry to linked-list
    let prev_data: &mut[u8] = &mut settle_b.try_borrow_mut_data()?;
    let (prev_top, _prev_pages) = mut_array_refs![prev_data, size_of::<AccountsHeader>(); .. ;];
    let prev_header: &mut [AccountsHeader] = cast_slice_mut(prev_top);
    prev_header[0].set_next(settle_n.key);

    let settle_data: &mut[u8] = &mut settle_n.try_borrow_mut_data()?;
    let (settle_top, settle_pages) = mut_array_refs![settle_data, size_of::<AccountsHeader>(); .. ;];
    let settle_header: &mut [AccountsHeader] = cast_slice_mut(settle_top);
    settle_header[0] = AccountsHeader {
        market: market_key,
        prev: *settle_b.key,
        next: Pubkey::default(),
        items: 0,
    };
    let settle_slab = SlabPageAlloc::new(settle_pages);
    settle_slab.setup_page_table();
    settle_slab.allocate::<CritMapHeader, AnyNode>(SettleDT::AccountMap as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");
    settle_slab.allocate::<SlabVec, AccountEntry>(SettleDT::Account as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");

    market_state.settle_a = *settle_b.key;
    market_state.settle_b = *settle_n.key;
    market_state.log_rollover = false;

    Ok(())
}

fn log_reimburse(
    market: &Market,
    state: &mut MarketState,
    user: &AccountInfo,
) -> anchor_lang::Result<()> {
    state.log_deposit_balance = state.log_deposit_balance.checked_sub(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;

    let mut user_lamports = user.lamports();
    user_lamports = user_lamports.checked_add(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;
    **user.lamports.borrow_mut() = user_lamports;

    Ok(())
}

fn log_close<'info>(
    state: &mut MarketState,
    settle: &AccountInfo<'info>,
    log_prev: &AccountInfo<'info>,
    log_next: &AccountInfo<'info>,
) -> anchor_lang::Result<u64> {
    let log_data: &mut[u8] = &mut settle.try_borrow_mut_data()?;
    let (header, _page_table) = mut_array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
    let settle_header: &mut [AccountsHeader] = cast_slice_mut(header);
    if settle_header[0].items > 0 {
        msg!("Attempted to close settlement log with remaining accounts");
        return Err(error!(ErrorCode::SettlementLogNotEmpty));
    }
    if settle_header[0].prev == Pubkey::default() {
        msg!("Cannot remove first settlement log");
        return Err(error!(ErrorCode::InvalidAccount));
    }
    if settle_header[0].next == Pubkey::default() {
        msg!("Cannot remove last settlement log");
        return Err(error!(ErrorCode::InvalidAccount));
    }
    let log_prev_data: &mut[u8] = &mut log_prev.try_borrow_mut_data()?;
    let log_next_data: &mut[u8] = &mut log_next.try_borrow_mut_data()?;
    let (prev_header, _page_table) = mut_array_refs![log_prev_data, size_of::<AccountsHeader>(); .. ;];
    let (next_header, _page_table) = mut_array_refs![log_next_data, size_of::<AccountsHeader>(); .. ;];
    let settle_prev: &mut [AccountsHeader] = cast_slice_mut(prev_header);
    let settle_next: &mut [AccountsHeader] = cast_slice_mut(next_header);

    // Validate settlement log chain before removing entry
    verify_matching_accounts(&settle_header[0].prev, &log_prev.key(), Some(String::from("Previous account does not match")))?;
    verify_matching_accounts(&settle_header[0].next, &log_next.key(), Some(String::from("Next account does not match")))?;
    verify_matching_accounts(&settle_prev[0].next, settle.key, Some(String::from("Previous account not linked")))?;
    verify_matching_accounts(&settle_next[0].prev, settle.key, Some(String::from("Next account not linked")))?;

    settle_prev[0].next = log_next.key();
    settle_next[0].prev = log_prev.key();
    if state.settle_a == *settle.key {
        state.settle_a = log_next.key();
        state.settle_b = settle_next[0].next;
    } else if state.settle_b == *settle.key {
        state.settle_b = log_next.key();
    }

    let log_lamports = settle.lamports();
    **settle.lamports.borrow_mut() = 0;

    Ok(log_lamports)
}

fn valid_order(order_type: OrderDT, leaf: &LeafNode, user_key: &Pubkey, sl: &SlabPageAlloc, expired_orders: &mut Vec<u128>, clock_ts: i64) -> bool {
    let order = sl.index::<Order>(order_type as u16, leaf.slot() as usize);
    let valid_expiry: bool = order.expiry == 0 || order.expiry < clock_ts;      // Check expiry timestamp if needed
    // TODO: Update before release
    let valid_user: bool = leaf.owner() != *user_key;                           // Prevent trades between the same user
    let valid = valid_expiry && valid_user;
    /*msg!("Atellix: Found {} [{}] {} @ {} Exp: {} Key: {} OK: {}",
        match order_type { OrderDT::BidOrder => "Bid", OrderDT::AskOrder => "Ask", _ => unreachable!() },
        leaf.slot().to_string(), order.amount().to_string(), Order::price(leaf.key()).to_string(),
        order.expiry.to_string(), leaf.owner().to_string(), valid.to_string(),
    );*/
    if !valid_expiry {
        expired_orders.push(leaf.key());
    }
    valid
}

fn perform_transfer<'info>(
    accounts: &[AccountInfo<'info>],
    mint_type: MintType,
    ast_offset: usize,
    amount: u64,
    preview: bool,
    from: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    auth: &AccountInfo<'info>,
    spl_prog: &AccountInfo<'info>,
) -> anchor_lang::Result<()> {
    if mint_type == MintType::SPLToken {
        if preview {
            let token_acct = load_struct::<SPL_TokenAccount>(&from.clone())?;
            if token_acct.is_frozen() {
                return Err(ErrorCode::ExternalError.into());
            }
            if token_acct.amount < amount {
                return Err(ErrorCode::InsufficientTokens.into());
            }
            return Ok(());
        } else {
            let in_accounts = SPL_Transfer {
                from: from.clone(),
                to: to.clone(),
                authority: auth.clone(),
            };
            let in_ctx = CpiContext::new(spl_prog.clone(), in_accounts);
            token::transfer(in_ctx, amount)?;
            return Ok(());
        }
    } else if mint_type == MintType::AtxSecurityToken {
        if preview {
            
        } else {
            let ast_prog = accounts.get(ast_offset).unwrap().to_account_info();
            require!(*ast_prog.key == security_token::ID, ErrorCode::InvalidParameters);
            let in_accounts = AST_Transfer {
                from: from.clone(),
                from_auth: accounts.get(ast_offset + 1).unwrap().to_account_info(),
                to: to.clone(),
                to_auth: accounts.get(ast_offset + 2).unwrap().to_account_info(),
                user: auth.clone(),
            };
            let in_ctx = CpiContext::new(ast_prog, in_accounts);
            security_token::cpi::transfer(in_ctx, amount)?;
            return Ok(());
        }
    }
    Err(error!(ErrorCode::InvalidParameters))
}

fn perform_signed_transfer<'info>(
    accounts: &[AccountInfo<'info>],
    signer: &'_ [&'_ [&'_ [u8]]],
    mint_type: MintType,
    ast_offset: usize,
    amount: u64,
    from: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    auth: &AccountInfo<'info>,
    spl_prog: &AccountInfo<'info>,
) -> anchor_lang::Result<()> {
    if mint_type == MintType::SPLToken {
        let in_accounts = SPL_Transfer {
            from: from.clone(),
            to: to.clone(),
            authority: auth.clone(),
        };
        let in_ctx = CpiContext::new_with_signer(spl_prog.clone(), in_accounts, signer);
        token::transfer(in_ctx, amount)?;
        return Ok(());
    } else if mint_type == MintType::AtxSecurityToken {
        let ast_prog = accounts.get(ast_offset).unwrap().to_account_info();
        require!(*ast_prog.key == security_token::ID, ErrorCode::InvalidParameters);
        let in_accounts = AST_Transfer {
            from: from.clone(),
            from_auth: accounts.get(ast_offset + 1).unwrap().to_account_info(),
            to: to.clone(),
            to_auth: accounts.get(ast_offset + 2).unwrap().to_account_info(),
            user: auth.clone(),
        };
        let in_ctx = CpiContext::new_with_signer(ast_prog, in_accounts, signer);
        security_token::cpi::transfer(in_ctx, amount)?;
        return Ok(());
    }
    Err(error!(ErrorCode::InvalidParameters))
}

fn log_trade(
    tlog: &mut SlabPageAlloc,
    event_type: u128,
    action_id: u64,
    market: &Pubkey,
    maker_order_id: u128,
    maker_filled: bool,
    maker: &Pubkey,
    taker: &Pubkey,
    taker_side: u8,
    amount: u64,
    price: u64,
    ts: i64,
) -> anchor_lang::Result<()> {
    let trade_header = tlog.header_mut::<TradeLogHeader>(0);
    let log_index = trade_header.trade_count.rem_euclid(trade_header.entry_max);
    let next_trade = trade_header.trade_count.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
    trade_header.trade_count = next_trade;
    let log_entry = tlog.index_mut::<TradeEntry>(0, log_index as usize);
    log_entry.event_type = event_type;
    log_entry.action_id = action_id;
    log_entry.trade_id = next_trade;
    log_entry.maker_order_id = maker_order_id;
    log_entry.maker_filled = maker_filled;
    log_entry.maker = *maker;
    log_entry.taker = *taker;
    log_entry.taker_side = taker_side;
    log_entry.amount = amount;
    log_entry.price = price;
    log_entry.ts = ts;

    msg!("atellix-log");
    emit!(MatchEvent {
        event_type: event_type,
        action_id: action_id,
        trade_id: next_trade,
        market: *market,
        maker_order_id: maker_order_id,
        maker_filled: maker_filled,
        maker: *maker,
        taker: *taker,
        taker_side: taker_side,
        amount: amount,
        price: price,
        ts: ts,
    });
    Ok(())
}

#[program]
pub mod aqua_dex {
    use super::*;

    pub fn store_metadata(ctx: Context<UpdateMetadata>,
        inp_program_name: String,
        inp_developer_name: String,
        inp_developer_url: String,
        inp_source_url: String,
        inp_verify_url: String,
    ) -> anchor_lang::Result<()> {
        let md = &mut ctx.accounts.program_info;
        md.semvar_major = VERSION_MAJOR;
        md.semvar_minor = VERSION_MINOR;
        md.semvar_patch = VERSION_PATCH;
        md.program = ctx.accounts.program.key();
        md.program_name = inp_program_name;
        md.developer_name = inp_developer_name;
        md.developer_url = inp_developer_url;
        md.source_url = inp_source_url;
        md.verify_url = inp_verify_url;
        msg!("Program: {}", ctx.accounts.program.key.to_string());
        msg!("Program Name: {}", md.program_name.as_str());
        msg!("Version: {}.{}.{}", VERSION_MAJOR.to_string(), VERSION_MINOR.to_string(), VERSION_PATCH.to_string());
        msg!("Developer Name: {}", md.developer_name.as_str());
        msg!("Developer URL: {}", md.developer_url.as_str());
        msg!("Source URL: {}", md.source_url.as_str());
        msg!("Verify URL: {}", md.verify_url.as_str());
        Ok(())
    }

    pub fn create_market<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, CreateMarket<'info>>,
        inp_agent_nonce: u8,
        inp_mkt_vault_nonce: u8,
        inp_prc_vault_nonce: u8,
        inp_mkt_decimals: u8,
        inp_prc_decimals: u8,
        inp_mkt_mint_type: u8,
        inp_prc_mint_type: u8,
        inp_manager_withdraw: bool,
        inp_manager_cancel: bool,
        inp_expire_enable: bool,
        inp_expire_min: i64,
        inp_min_quantity: u64,
        inp_taker_fee: u32,
        inp_log_fee: u64,
        inp_log_rebate: u64,
        inp_log_reimburse: u64,
        inp_mkt_vault_uuid: u128,
        inp_prc_vault_uuid: u128,
    ) -> anchor_lang::Result<()> {
        msg!("Begin Market Setup");
        let clock = Clock::get()?;
        let clock_ts = clock.unix_timestamp;

        let acc_market = &ctx.accounts.market.to_account_info();
        let acc_state = &ctx.accounts.state.to_account_info();
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_manager = &ctx.accounts.manager.to_account_info();
        let acc_mkt_mint = &ctx.accounts.mkt_mint.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_mint = &ctx.accounts.prc_mint.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();

        let mkt_mint_type: MintType = MintType::try_from(inp_mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
        let prc_mint_type: MintType = MintType::try_from(inp_prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;

        if mkt_mint_type == MintType::AtxSecurityToken || prc_mint_type == MintType::AtxSecurityToken {
            let acc_ast = ctx.remaining_accounts.get(0).unwrap().to_account_info();
            require!(*acc_ast.key == security_token::ID, ErrorCode::InvalidParameters);
        }

        if mkt_mint_type == MintType::SPLToken {
            // Verify associated token (market)
            let derived_mkt_vault = Pubkey::create_program_address(
                &[&acc_agent.key.to_bytes(), &Token::id().to_bytes(), &acc_mkt_mint.key.to_bytes(), &[inp_mkt_vault_nonce]],
                &AssociatedToken::id(),
            ).map_err(|_| ErrorCode::InvalidDerivedAccount)?;
            if derived_mkt_vault != *acc_mkt_vault.key {
                msg!("Invalid market token vault");
                return Err(ErrorCode::InvalidDerivedAccount.into());
            }
        }

        if prc_mint_type == MintType::SPLToken {
            // Verify associated token (pricing)
            let derived_prc_vault = Pubkey::create_program_address(
                &[&acc_agent.key.to_bytes(), &Token::id().to_bytes(), &acc_prc_mint.key.to_bytes(), &[inp_prc_vault_nonce]],
                &AssociatedToken::id(),
            ).map_err(|_| ErrorCode::InvalidDerivedAccount)?;
            if derived_prc_vault != *acc_prc_vault.key {
                msg!("Invalid pricing token vault");
                return Err(ErrorCode::InvalidDerivedAccount.into());
            }
        }

        // Check expiration parameters
        if inp_expire_min < 1 {
            msg!("Invalid order expiration duration");
            return Err(ErrorCode::InvalidParameters.into());
        }

        // Create token vaults
        let acc_spl = &ctx.accounts.spl_token_prog.to_account_info();
        let acc_sys = &ctx.accounts.system_program.to_account_info();
        let acc_rent = &ctx.accounts.system_rent.to_account_info();

        if mkt_mint_type == MintType::SPLToken {
            let instr1 = Instruction {
                program_id: AssociatedToken::id(),
                accounts: vec![
                    AccountMeta::new(*acc_manager.key, true),
                    AccountMeta::new(*acc_mkt_vault.key, false),
                    AccountMeta::new_readonly(*acc_agent.key, false),
                    AccountMeta::new_readonly(*acc_mkt_mint.key, false),
                    AccountMeta::new_readonly(solana_program::system_program::id(), false),
                    AccountMeta::new_readonly(Token::id(), false),
                    AccountMeta::new_readonly(sysvar::rent::id(), false),
                ],
                data: vec![],
            };
            let res1 = invoke(&instr1, &[
                acc_manager.clone(), acc_mkt_vault.clone(), acc_agent.clone(), acc_mkt_mint.clone(),
                acc_spl.clone(), acc_sys.clone(), acc_rent.clone(),
            ]);
            if res1.is_err() {
                msg!("Create associated token failed for market token");
                return Err(ErrorCode::ExternalError.into());
            }
        } else if mkt_mint_type == MintType::AtxSecurityToken {
            msg!("Create security token vault");
            let seeds = &[acc_market.key.as_ref(), &[inp_agent_nonce]];
            let signer = &[&seeds[..]];
            let in_accounts = AST_CreateAccount {
                account: acc_mkt_vault.clone(),
                mint: acc_mkt_mint.clone(),
                owner: acc_agent.clone(),
                fee_payer: acc_manager.clone(),
                create_auth: ctx.remaining_accounts.get(0).unwrap().to_account_info(),
                close_auth: acc_agent.clone(),
                system_program: acc_sys.clone(),
            };
            let acc_ast = ctx.remaining_accounts.get(0).unwrap().to_account_info();
            let in_ctx = CpiContext::new_with_signer(acc_ast, in_accounts, signer);
            security_token::cpi::create_account(in_ctx, inp_mkt_vault_uuid)?;
        }

        if prc_mint_type == MintType::SPLToken {
            let instr2 = Instruction {
                program_id: AssociatedToken::id(),
                accounts: vec![
                    AccountMeta::new(*acc_manager.key, true),
                    AccountMeta::new(*acc_prc_vault.key, false),
                    AccountMeta::new_readonly(*acc_agent.key, false),
                    AccountMeta::new_readonly(*acc_prc_mint.key, false),
                    AccountMeta::new_readonly(solana_program::system_program::id(), false),
                    AccountMeta::new_readonly(Token::id(), false),
                    AccountMeta::new_readonly(sysvar::rent::id(), false),
                ],
                data: vec![],
            };
            let res2 = invoke(&instr2, &[
                acc_manager.clone(), acc_prc_vault.clone(), acc_agent.clone(), acc_prc_mint.clone(),
                acc_spl.clone(), acc_sys.clone(), acc_rent.clone(),
            ]);
            if res2.is_err() {
                msg!("Create associated token failed for pricing token");
                return Err(ErrorCode::ExternalError.into());
            }
        } else if mkt_mint_type == MintType::AtxSecurityToken && prc_mint_type == MintType::AtxSecurityToken {
            msg!("SPL mint required");
            return Err(ErrorCode::InvalidParameters.into());
        } else if prc_mint_type == MintType::AtxSecurityToken {
            let seeds = &[acc_market.key.as_ref(), &[inp_agent_nonce]];
            let signer = &[&seeds[..]];
            let mut offset: usize = 0;
            if mkt_mint_type == MintType::AtxSecurityToken {
                offset = 1;
            }
            let in_accounts = AST_CreateAccount {
                account: acc_prc_vault.clone(),
                mint: acc_prc_mint.clone(),
                owner: acc_agent.clone(),
                fee_payer: acc_manager.clone(),
                create_auth: ctx.remaining_accounts.get(offset).unwrap().to_account_info(),
                close_auth: acc_agent.clone(),
                system_program: acc_sys.clone(),
            };
            let acc_ast = ctx.remaining_accounts.get(0).unwrap().to_account_info();
            let in_ctx = CpiContext::new_with_signer(acc_ast, in_accounts, signer);
            security_token::cpi::create_account(in_ctx, inp_prc_vault_uuid)?;
        }

        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_trade_log = &ctx.accounts.trade_log.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();

        let market = Market {
            active: true,
            manager_withdraw: inp_manager_withdraw,
            manager_cancel: inp_manager_cancel,
            expire_enable: inp_expire_enable,
            expire_min: inp_expire_min,
            min_quantity: inp_min_quantity,
            log_fee: inp_log_fee,
            log_rebate: inp_log_rebate,
            log_reimburse: inp_log_reimburse,
            taker_fee: inp_taker_fee,
            state: *acc_state.key,
            trade_log: *acc_trade_log.key,
            agent: *acc_agent.key,
            agent_nonce: inp_agent_nonce,
            manager: *acc_manager.key,
            mkt_mint: *acc_mkt_mint.key,
            mkt_vault: *acc_mkt_vault.key,
            mkt_nonce: inp_mkt_vault_nonce,
            mkt_decimals: inp_mkt_decimals,
            mkt_mint_type: inp_mkt_mint_type,
            prc_mint: *acc_prc_mint.key,
            prc_vault: *acc_prc_vault.key,
            prc_nonce: inp_prc_vault_nonce,
            prc_decimals: inp_prc_decimals,
            prc_mint_type: inp_prc_mint_type,
            orders: *acc_orders.key,
            settle_0: *acc_settle1.key,
        };
        msg!("Atellix: Store Market Data");
        store_struct::<Market>(&market, acc_market)?;

        let state = MarketState {
            settle_a: *acc_settle1.key,
            settle_b: *acc_settle2.key,
            log_rollover: false,
            log_deposit_balance: 0,
            action_counter: 0,
            trade_counter: 0,
            order_counter: 0,
            active_bid: 0,
            active_ask: 0,
            mkt_vault_balance: 0,
            mkt_order_balance: 0,
            mkt_user_vault_balance: 0,
            mkt_log_balance: 0,
            prc_vault_balance: 0,
            prc_order_balance: 0,
            prc_user_vault_balance: 0,
            prc_log_balance: 0,
            prc_fees_balance: 0,
            last_ts: clock_ts,
            last_price: 0,
        };
        msg!("Atellix: Store Market State");
        store_struct::<MarketState>(&state, acc_state)?;

        msg!("Atellix: Allocate Orderbook");
        let order_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let order_slab = SlabPageAlloc::new(order_data);
        order_slab.setup_page_table();
        order_slab.allocate::<CritMapHeader, AnyNode>(OrderDT::BidOrderMap as u16, MAX_ORDERS as usize).expect("Failed to allocate");
        order_slab.allocate::<CritMapHeader, AnyNode>(OrderDT::AskOrderMap as u16, MAX_ORDERS as usize).expect("Failed to allocate");
        order_slab.allocate::<SlabVec, Order>(OrderDT::BidOrder as u16, MAX_ORDERS as usize).expect("Failed to allocate");
        order_slab.allocate::<SlabVec, Order>(OrderDT::AskOrder as u16, MAX_ORDERS as usize).expect("Failed to allocate");

        msg!("Atellix: Allocate Trade Log");
        let trade_data: &mut[u8] = &mut acc_trade_log.try_borrow_mut_data()?;
        let trade_slab = SlabPageAlloc::new(trade_data);
        trade_slab.setup_page_table();
        trade_slab.allocate::<TradeLogHeader, TradeEntry>(0, MAX_TRADES as usize).expect("Failed to allocate");
        let trade_header = trade_slab.header_mut::<TradeLogHeader>(0);
        trade_header.trade_count = 0;
        trade_header.entry_max = MAX_TRADES as u64;

        msg!("Atellix: Allocate Settlement Log 1");
        let settle1_data: &mut[u8] = &mut acc_settle1.try_borrow_mut_data()?;
        let (settle1_top, settle1_pages) = mut_array_refs![settle1_data, size_of::<AccountsHeader>(); .. ;];
        let settle1_header: &mut [AccountsHeader] = cast_slice_mut(settle1_top);
        settle1_header[0] = AccountsHeader {
            market: *acc_market.key,
            prev: Pubkey::default(),
            next: *acc_settle2.key,
            items: 0,
        };
        let settle1_slab = SlabPageAlloc::new(settle1_pages);
        settle1_slab.setup_page_table();
        settle1_slab.allocate::<CritMapHeader, AnyNode>(SettleDT::AccountMap as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");
        settle1_slab.allocate::<SlabVec, AccountEntry>(SettleDT::Account as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");

        msg!("Atellix: Allocate Settlement Log 2");
        let settle2_data: &mut[u8] = &mut acc_settle2.try_borrow_mut_data()?;
        let (settle2_top, settle2_pages) = mut_array_refs![settle2_data, size_of::<AccountsHeader>(); .. ;];
        let settle2_header: &mut [AccountsHeader] = cast_slice_mut(settle2_top);
        settle2_header[0] = AccountsHeader {
            market: *acc_market.key,
            prev: *acc_settle2.key,
            next: Pubkey::default(),
            items: 0,
        };
        let settle2_slab = SlabPageAlloc::new(settle2_pages);
        settle2_slab.setup_page_table();
        settle2_slab.allocate::<CritMapHeader, AnyNode>(SettleDT::AccountMap as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");
        settle2_slab.allocate::<SlabVec, AccountEntry>(SettleDT::Account as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");

        //msg!("Atellix: Account Entry Size: {}", size_of::<AccountEntry>());

        msg!("Atellix: Created AquaDEX market");

        Ok(())
    }

    pub fn limit_bid<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, OrderContext<'info>>,
        inp_quantity: u64,
        inp_price: u64,
        inp_post: bool,     // Post the order order to the orderbook, otherwise fill based on parameter below
        inp_fill: bool,     // Require orders that are not posted to be filled completely
        inp_expires: i64,   // Unix timestamp for order expiration (must be in the future, must exceed minimum duration)
        inp_preview: bool,  // Preview execution and check taker token balance, but do not perform transfer
        inp_rollover: bool, // Perform settlement log rollover
    ) -> anchor_lang::Result<TradeResult> {
        require!(inp_quantity > 0, ErrorCode::InvalidParameters);
        require!(inp_price > 0, ErrorCode::InvalidParameters);
        let clock = Clock::get()?;
        let clock_ts = clock.unix_timestamp;

        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_user = &ctx.accounts.user.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        if inp_post && inp_fill {
            msg!("Require fill cannot be used with order posting");
            return Err(ErrorCode::InvalidParameters.into());
        }
        if !market.active {
            msg!("Market closed");
            return Err(ErrorCode::MarketClosed.into());
        }
        require!(inp_quantity > 0 && inp_quantity >= market.min_quantity, ErrorCode::QuantityBelowMinimum);

        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let s1 = verify_matching_accounts(&market_state.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market_state.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into());
        }

        // Append a settlement log account
        let state_upd = &mut ctx.accounts.state;
        if inp_rollover && !inp_preview {
            if !state_upd.log_rollover {
                // Another market participant already appended a new log account (please retry transaction)
                msg!("Please update market data and retry");
                return Err(ErrorCode::RetrySettlementAccount.into());
            }
            let av = ctx.remaining_accounts;
            let new_settlement_log = av.get(0).unwrap();
            let market_pk: Pubkey = market.key();
            log_rollover(state_upd, market_pk, acc_settle2, new_settlement_log)?;
            log_reimburse(market, state_upd, acc_user)?;
            let mut market_lamports = state_upd.to_account_info().lamports();
            market_lamports = market_lamports.checked_sub(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;
            **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;
        }

        // Check expiration parameters
        let mut expiry: i64 = 0;
        // If expire timestamp is 0 then order does not expire
        if market.expire_enable && inp_expires != 0 {
            let expire_dur = inp_expires.checked_sub(clock_ts).ok_or(error!(ErrorCode::Overflow))?;
            if expire_dur <= 0 {
                msg!("Order already expired");
                return Err(ErrorCode::InvalidParameters.into());
            }
            if expire_dur < market.expire_min {
                msg!("Order expires before minimum duration of {} seconds", market.expire_min.to_string());
                return Err(ErrorCode::InvalidParameters.into());
            }
            expiry = inp_expires;
        }

        msg!("Atellix: Limit Bid: {} @ {}", inp_quantity.to_string(), inp_price.to_string());

        let mkt_decimal_base: u64 = 10;
        let mkt_decimal_factor: u64 = mkt_decimal_base.pow(market.mkt_decimals as u32);
        let mut tokens_in_calc: u128 = (inp_price as u128).checked_mul(inp_quantity as u128).ok_or(error!(ErrorCode::Overflow))?;
        tokens_in_calc = tokens_in_calc.checked_div(mkt_decimal_factor as u128).ok_or(error!(ErrorCode::Overflow))?;
        let tokens_in: u64 = u64::try_from(tokens_in_calc).map_err(|_| error!(ErrorCode::Overflow))?;
        if !inp_preview {
            state_upd.action_counter = state_upd.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_in).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(tokens_in).ok_or(error!(ErrorCode::Overflow))?;
        }

        let orderbook_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let ob = SlabPageAlloc::new(orderbook_data);

        // Check if order can be filled
        let mut tokens_to_fill: u64 = inp_quantity;
        let mut tokens_filled: u64 = 0;
        let mut tokens_paid: u64 = 0;
        let mut tokens_fee: u64 = 0;
        let mut expired_orders = Vec::new();
        let acc_trade_log = &ctx.accounts.trade_log.to_account_info();
        verify_matching_accounts(&market.trade_log, &acc_trade_log.key, Some(String::from("Invalid trade log")))?;
        let trade_data: &mut[u8] = &mut acc_trade_log.try_borrow_mut_data()?;
        let tlog = SlabPageAlloc::new(trade_data);
        loop {
            let node_res = map_predicate_min(ob, DT::AskOrder, |sl, leaf|
                valid_order(OrderDT::AskOrder, leaf, acc_user.key, sl, &mut expired_orders, clock_ts)
            );
            if node_res.is_none() {
                msg!("Atellix: No Match");
                break;
            }
            let posted_node = node_res.unwrap();
            let posted_order = ob.index::<Order>(OrderDT::AskOrder as u16, posted_node.slot() as usize);
            let posted_qty = posted_order.amount;
            let posted_price = Order::price(posted_node.key());
            msg!("Atellix: Matched Ask [{}] {} @ {}", posted_node.slot().to_string(), posted_qty.to_string(), posted_price.to_string());
            if posted_price <= inp_price {
                // Fill order
                if posted_qty == tokens_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_part.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            207368829214137069500050352632921761096, // solana/program/aqua-dex/limit_bid/match/exact
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::AskOrder, posted_node.key())?;
                        Order::free_index(ob, DT::AskOrder, posted_node.slot())?;
                        state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    }
                    break;
                } else if posted_qty < tokens_to_fill {   // Match the entire order and continue
                    tokens_to_fill = tokens_to_fill.checked_sub(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(posted_qty, posted_price, mkt_decimal_factor)?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", posted_qty.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            227168296477409633500015956081940497570, // solana/program/aqua-dex/limit_bid/match/entire
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::AskOrder, posted_node.key())?;
                        Order::free_index(ob, DT::AskOrder, posted_node.slot())?;
                        state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    }
                } else if posted_qty > tokens_to_fill {   // Match part of the order
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_to_fill.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            94062763214239030578622318919331863353, // solana/program/aqua-dex/limit_bid/match/partial
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            false,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        let new_amount = posted_qty.checked_sub(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        ob.index_mut::<Order>(OrderDT::AskOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    }
                    break;
                }
            } else {
                // Best price beyond limit price
                break;
            }
        }

        msg!("Atellix: Fee: {}", tokens_fee.to_string());

        if !inp_preview {
            let mut expired_count: u32 = 0;
            if expired_orders.len() > 0 {
                loop {
                    if expired_orders.len() == 0 || expired_count == MAX_EXPIRATIONS {
                        break;
                    }
                    let expired_id: u128 = expired_orders.pop().unwrap();
                    let expire_leaf = map_get(ob, DT::AskOrder, expired_id).unwrap();
                    let expire_order = *ob.index::<Order>(OrderDT::AskOrder as u16, expire_leaf.slot() as usize);
                    let expire_amount: u64 = expire_order.amount();
                    msg!("Atellix: Expired Order[{}] - Owner: {} {} @ {}",
                        expire_leaf.slot().to_string(),
                        expire_leaf.owner().to_string(),
                        expire_order.amount().to_string(),
                        Order::price(expire_leaf.key()).to_string(),
                    );
                    msg!("atellix-log");
                    emit!(ExpireEvent {
                        event_type: 16332991664789055110548783525139174482, // solana/program/aqua-dex/expire_event
                        action_id: state_upd.action_counter,
                        market: market.key(),
                        owner: expire_leaf.owner(),
                        order_side: Side::Ask as u8,
                        order_id: expired_id,
                        price: Order::price(expire_leaf.key()),
                        quantity: expire_amount,
                        tokens: expire_amount,
                    });
                    log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &expire_leaf.owner(), true, expire_amount)?; // No multiply for Ask order
                    map_remove(ob, DT::AskOrder, expire_leaf.key())?;
                    Order::free_index(ob, DT::AskOrder, expire_leaf.slot())?;
                    state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                    expired_count = expired_count + 1;
                }
            }
        }

        let mut result = TradeResult { tokens_received: tokens_filled, posted_quantity: 0, tokens_sent: 0, tokens_fee: tokens_fee, order_id: 0 };

        // Add order to orderbook if not filled
        let tokens_remaining = inp_quantity.checked_sub(tokens_filled).ok_or(error!(ErrorCode::Overflow))?;
        if tokens_remaining > 0 && inp_fill {
            msg!("Order not filled");
            return Err(ErrorCode::OrderNotFilled.into());
        }
        if tokens_remaining > 0 && inp_post {
            let mut order_id: u128 = u128::MAX;
            let mut order_idx: u32 = 1;
            if !inp_preview {
                order_id = Order::new_key(state_upd, Side::Bid, inp_price);
                order_idx = Order::next_index(ob, DT::BidOrder)?;
                let order_node = LeafNode::new(order_id, order_idx, &acc_user.key);
                let order = Order { amount: tokens_remaining, expiry: expiry };
                let mut eviction_count: u32 = 0;
                loop {
                    let entry = map_insert(ob, DT::BidOrder, &order_node);
                    if entry.is_err() {
                        // Evict orders if necessary
                        if eviction_count == MAX_EVICTIONS {
                            msg!("Failed to add order");
                            return Err(ErrorCode::InternalError.into());
                        }
                        let evict_node = map_min(ob, DT::BidOrder).unwrap();
                        let evict_order = ob.index::<Order>(OrderDT::BidOrder as u16, evict_node.slot() as usize);
                        // Only evict if the price is better
                        if inp_price <= Order::price(evict_node.key()) {
                            msg!("Atellix: Orderbook Full - Price does not exceed evicted order");
                            return Err(ErrorCode::OrderbookFull.into());
                        }
                        let evict_amount: u64 = evict_order.amount();
                        msg!("Atellix: Evicting Order[{}] - Owner: {} {} @ {}",
                            evict_node.slot().to_string(),
                            evict_node.owner().to_string(),
                            evict_order.amount().to_string(),
                            Order::price(evict_node.key()).to_string(),
                        );
                        let evict_total = scale_price(evict_amount, Order::price(evict_node.key()), mkt_decimal_factor)?;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &evict_node.owner(), false, evict_total)?;
                        map_remove(ob, DT::BidOrder, evict_node.key())?;
                        Order::free_index(ob, DT::BidOrder, evict_node.slot())?;
                        state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        eviction_count = eviction_count + 1;
                    } else {
                        *ob.index_mut::<Order>(OrderDT::BidOrder.into(), order_idx as usize) = order;
                        state_upd.active_bid = state_upd.active_bid.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
                        break;
                    }
                }
            }
            let tokens_part = scale_price(tokens_remaining, inp_price, mkt_decimal_factor)?;
            tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
            result.set_posted_quantity(tokens_remaining);
            result.set_order_id(order_id);
            msg!("Atellix: Posted Bid [{}] {} @ {}", order_idx.to_string(), tokens_remaining.to_string(), inp_price.to_string());
        }
        let discount = tokens_in.checked_sub(tokens_paid).ok_or(error!(ErrorCode::Overflow))?;
        msg!("Atellix: Discount: {}", discount.to_string());
        let mut total_cost = tokens_in.checked_sub(discount).ok_or(error!(ErrorCode::Overflow))?;
        total_cost = total_cost.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
        if !inp_preview {
            state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_sub(discount).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.prc_order_balance = state_upd.prc_order_balance.checked_sub(discount).ok_or(error!(ErrorCode::Overflow))?;

            // Apply fees
            state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.prc_fees_balance = state_upd.prc_fees_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;

            /*msg!("Atellix: Pricing Token Vault Deposit: {}", total_cost.to_string());
            msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
                state_upd.prc_vault_balance,
                state_upd.prc_order_balance,
            );*/

            // Deposit lamports for settlement log space
            let mut user_lamports = ctx.accounts.user.lamports();
            user_lamports = user_lamports.checked_sub(market.log_fee).ok_or(error!(ErrorCode::Overflow))?;
            **ctx.accounts.user.lamports.borrow_mut() = user_lamports;
            let mut market_lamports = state_upd.to_account_info().lamports();
            market_lamports = market_lamports.checked_add(market.log_fee).ok_or(error!(ErrorCode::Overflow))?;
            **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;
            state_upd.log_deposit_balance = state_upd.log_deposit_balance.checked_add(market.log_fee).ok_or(error!(ErrorCode::Overflow))?;
        }

        // Send tokens to the vault
        let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
        perform_transfer(ctx.remaining_accounts, mint_type, 0, total_cost, inp_preview,
            &ctx.accounts.user_prc_token.to_account_info(),  // From
            &ctx.accounts.prc_vault.to_account_info(),       // To
            &ctx.accounts.user.to_account_info(),            // Auth
            &ctx.accounts.spl_token_prog.to_account_info(),  // SPL Token Program
        )?;
        result.set_tokens_sent(total_cost);

        if tokens_filled > 0 && !inp_preview {
            // Withdraw tokens from the vault
            state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_sub(tokens_filled).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_sub(tokens_filled).ok_or(error!(ErrorCode::Overflow))?;

            /*msg!("Atellix: Market Token Vault Withdraw: {}", tokens_filled.to_string());
            msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
                state_upd.mkt_vault_balance,
                state_upd.mkt_order_balance,
            );*/

            let seeds = &[market.to_account_info().key.as_ref(), &[market.agent_nonce]];
            let signer = &[&seeds[..]];
            let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
            perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, tokens_filled,
                &ctx.accounts.mkt_vault.to_account_info(),          // From
                &ctx.accounts.user_mkt_token.to_account_info(),     // To
                &ctx.accounts.agent.to_account_info(),              // Auth
                &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
            )?;
        }
        if *acc_result.key != *acc_user.key {
            store_struct::<TradeResult>(&result, acc_result)?;
        }

        if !inp_preview {
            msg!("atellix-log");
            emit!(OrderEvent {
                event_type: 58862986463747312203336335289809479007, // solana/program/aqua-dex/limit_bid/order
                action_id: state_upd.action_counter,
                market: market.key(),
                user: acc_user.key(),
                market_token: ctx.accounts.user_mkt_token.key(),
                pricing_token: ctx.accounts.user_prc_token.key(),
                order_id: result.order_id,
                order_side: Side::Bid as u8,
                filled: tokens_remaining == 0,
                tokens_received: result.tokens_received,
                tokens_sent: result.tokens_sent,
                tokens_fee: result.tokens_fee,
                posted: result.posted_quantity > 0,
                posted_quantity: result.posted_quantity,
                order_price: inp_price,
                order_quantity: inp_quantity,
                expires: expiry,
            });
        }

        Ok(result)
    }

    pub fn limit_ask<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, OrderContext<'info>>,
        inp_quantity: u64,
        inp_price: u64,
        inp_post: bool,     // Post the order order to the orderbook, otherwise fill based on parameter below
        inp_fill: bool,     // Require orders that are not posted to be filled completely
        inp_expires: i64,   // Unix timestamp for order expiration (must be in the future, must exceed minimum duration)
        inp_preview: bool,  // Preview mode
        inp_rollover: bool, // Perform settlement log rollover
    ) -> anchor_lang::Result<TradeResult> {
        require!(inp_quantity > 0, ErrorCode::InvalidParameters);
        require!(inp_price > 0, ErrorCode::InvalidParameters);
        let clock = Clock::get()?;
        let clock_ts = clock.unix_timestamp;

        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_user = &ctx.accounts.user.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        if inp_post && inp_fill {
            msg!("Require fill cannot be used with order posting");
            return Err(ErrorCode::InvalidParameters.into());
        }
        if !market.active {
            msg!("Market closed");
            return Err(ErrorCode::MarketClosed.into());
        }
        require!(inp_quantity > 0 && inp_quantity >= market.min_quantity, ErrorCode::QuantityBelowMinimum);

        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let s1 = verify_matching_accounts(&market_state.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market_state.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into()); 
        }

        // Append a settlement log account
        let state_upd = &mut ctx.accounts.state;
        if inp_rollover && !inp_preview {
            if !state_upd.log_rollover {
                // Another market participant already appended a new log account (please retry transaction)
                msg!("Please update market data and retry");
                return Err(ErrorCode::RetrySettlementAccount.into());
            }
            let av = ctx.remaining_accounts;
            let new_settlement_log = av.get(0).unwrap();
            let market_pk: Pubkey = market.key();
            log_rollover(state_upd, market_pk, acc_settle2, new_settlement_log)?;
            log_reimburse(market, state_upd, acc_user)?;
            let mut market_lamports = state_upd.to_account_info().lamports();
            market_lamports = market_lamports.checked_sub(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;
            **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;
        }

        // Check expiration parameters
        let mut expiry: i64 = 0;
        // If expire timestamp is 0 then order does not expire
        if market.expire_enable && inp_expires != 0 {
            let expire_dur = inp_expires.checked_sub(clock_ts).ok_or(error!(ErrorCode::Overflow))?;
            if expire_dur <= 0 {
                msg!("Order already expired");
                return Err(ErrorCode::InvalidParameters.into());
            }
            if expire_dur < market.expire_min {
                msg!("Order expires before minimum duration of {} seconds", market.expire_min.to_string());
                return Err(ErrorCode::InvalidParameters.into());
            }
            expiry = inp_expires;
        }

        msg!("Atellix: Limit Ask: {} @ {}", inp_quantity.to_string(), inp_price.to_string());

        if !inp_preview {
            state_upd.action_counter = state_upd.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(inp_quantity).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(inp_quantity).ok_or(error!(ErrorCode::Overflow))?;
        }

        let orderbook_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let ob = SlabPageAlloc::new(orderbook_data);

        let mkt_decimal_base: u64 = 10;
        let mkt_decimal_factor: u64 = mkt_decimal_base.pow(market.mkt_decimals as u32);

        // Check if order can be filled
        let mut tokens_to_fill: u64 = inp_quantity;
        let mut tokens_filled: u64 = 0;
        let mut tokens_received: u64 = 0;
        let mut tokens_fee: u64 = 0;
        let mut expired_orders = Vec::new();
        let acc_trade_log = &ctx.accounts.trade_log.to_account_info();
        verify_matching_accounts(&market.trade_log, &acc_trade_log.key, Some(String::from("Invalid trade log")))?;
        let trade_data: &mut[u8] = &mut acc_trade_log.try_borrow_mut_data()?;
        let tlog = SlabPageAlloc::new(trade_data);
        loop {
            let node_res = map_predicate_max(ob, DT::BidOrder, |sl, leaf|
                valid_order(OrderDT::BidOrder, leaf, acc_user.key, sl, &mut expired_orders, clock_ts)
            );
            if node_res.is_none() {
                msg!("Atellix: No Match");
                break;
            }
            let posted_node = node_res.unwrap();
            let posted_order = ob.index::<Order>(OrderDT::BidOrder as u16, posted_node.slot() as usize);
            let posted_qty = posted_order.amount;
            let posted_price = Order::price(posted_node.key());
            msg!("Atellix: Matched Bid [{}] {} @ {}", posted_node.slot().to_string(), posted_qty.to_string(), posted_price.to_string());
            if posted_price >= inp_price {
                // Fill order
                if posted_qty == tokens_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_part.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            325819153524900178081877579778492284961, // solana/program/aqua-dex/limit_ask/match/exact
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::BidOrder, posted_node.key())?;
                        Order::free_index(ob, DT::BidOrder, posted_node.slot())?;
                        state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, tokens_to_fill)?;
                    }
                    break;
                } else if posted_qty < tokens_to_fill {   // Match the entire order and continue
                    tokens_to_fill = tokens_to_fill.checked_sub(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(posted_qty, posted_price, mkt_decimal_factor)?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", posted_qty.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            114544905925567569513505448268003180936, // solana/program/aqua-dex/limit_ask/match/entire
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::BidOrder, posted_node.key())?;
                        Order::free_index(ob, DT::BidOrder, posted_node.slot())?;
                        state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, posted_qty)?;
                    }
                } else if posted_qty > tokens_to_fill {   // Match part of the order
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_to_fill.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            282510189476950091999666304965232626740, // solana/program/aqua-dex/limit_ask/match/partial
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            false,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        let new_amount = posted_qty.checked_sub(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        ob.index_mut::<Order>(OrderDT::BidOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, tokens_to_fill)?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                    }
                    break;
                }
            } else {
                // Best price beyond limit price
                break;
            }
        }

        msg!("Atellix: Fee: {}", tokens_fee.to_string());

        let mut expired_count: u32 = 0;
        if expired_orders.len() > 0 && !inp_preview {
            loop {
                if expired_orders.len() == 0 || expired_count == MAX_EXPIRATIONS {
                    break;
                }
                let expired_id: u128 = expired_orders.pop().unwrap();
                let expire_leaf = map_get(ob, DT::BidOrder, expired_id).unwrap();
                let expire_order = *ob.index::<Order>(OrderDT::BidOrder as u16, expire_leaf.slot() as usize);
                let expire_amount: u64 = expire_order.amount();
                msg!("Atellix: Expired Order[{}] - Owner: {} {} @ {}",
                    expire_leaf.slot().to_string(),
                    expire_leaf.owner().to_string(),
                    expire_order.amount().to_string(),
                    Order::price(expire_leaf.key()).to_string(),
                );
                let expire_price = Order::price(expire_leaf.key());
                let expire_total = scale_price(expire_amount, expire_price, mkt_decimal_factor)?;
                msg!("atellix-log");
                emit!(ExpireEvent {
                    event_type: 16332991664789055110548783525139174482, // solana/program/aqua-dex/expire_event
                    action_id: state_upd.action_counter,
                    market: market.key(),
                    owner: expire_leaf.owner(),
                    order_side: Side::Bid as u8,
                    order_id: expired_id,
                    price: Order::price(expire_leaf.key()),
                    quantity: expire_amount,
                    tokens: expire_total,
                });
                log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &expire_leaf.owner(), false, expire_total)?; // Total calculated
                map_remove(ob, DT::BidOrder, expire_leaf.key())?;
                Order::free_index(ob, DT::BidOrder, expire_leaf.slot())?;
                state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                expired_count = expired_count + 1;
            }
        }

        let mut result = TradeResult { tokens_received: 0, posted_quantity: 0, tokens_sent: inp_quantity, tokens_fee: tokens_fee, order_id: 0 };

        // Add order to orderbook if not filled
        let tokens_remaining = inp_quantity.checked_sub(tokens_filled).ok_or(error!(ErrorCode::Overflow))?;
        if tokens_remaining > 0 && inp_fill {
            msg!("Order not filled");
            return Err(ErrorCode::OrderNotFilled.into());
        }
        if tokens_remaining > 0 && inp_post {
            // Add order to orderbook
            let mut order_id: u128 = u128::MAX;
            let mut order_idx: u32 = 1;
            if !inp_preview {
                order_id = Order::new_key(state_upd, Side::Ask, inp_price);
                order_idx = Order::next_index(ob, DT::AskOrder)?;
                let order_node = LeafNode::new(order_id, order_idx, &acc_user.key);
                let order = Order { amount: tokens_remaining, expiry: expiry };
                let mut eviction_count: u32 = 0;
                loop {
                    let entry = map_insert(ob, DT::AskOrder, &order_node);
                    if entry.is_err() {
                        // Evict orders if necessary
                        if eviction_count == MAX_EVICTIONS {
                            msg!("Failed to add order");
                            return Err(ErrorCode::InternalError.into());
                        }
                        let evict_node = map_max(ob, DT::AskOrder).unwrap();
                        let evict_order = ob.index::<Order>(OrderDT::AskOrder as u16, evict_node.slot() as usize);
                        // Only evict if the price is better
                        if inp_price >= Order::price(evict_node.key()) {
                            msg!("Atellix: Orderbook Full - Price is not below evicted order");
                            return Err(ErrorCode::OrderbookFull.into());
                        }
                        let evict_amount: u64 = evict_order.amount();
                        msg!("Atellix: Evicting Order[{}] - Owner: {} {} @ {}",
                            evict_node.slot().to_string(),
                            evict_node.owner().to_string(),
                            evict_order.amount().to_string(),
                            Order::price(evict_node.key()).to_string(),
                        );
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &evict_node.owner(), true, evict_amount)?;
                        map_remove(ob, DT::AskOrder, evict_node.key())?;
                        Order::free_index(ob, DT::AskOrder, evict_node.slot())?;
                        state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        eviction_count = eviction_count + 1;
                    } else {
                        *ob.index_mut::<Order>(OrderDT::AskOrder.into(), order_idx as usize) = order;
                        state_upd.active_ask = state_upd.active_ask.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
                        break;
                    }
                }
            }
            result.set_posted_quantity(tokens_remaining);
            result.set_order_id(order_id);
            msg!("Atellix: Posted Ask [{}] {} @ {}", order_idx.to_string(), inp_quantity.to_string(), inp_price.to_string());
        }

        /*msg!("Atellix: Market Token Vault Deposit: {}", inp_quantity.to_string());
        msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
            state_upd.mkt_vault_balance,
            state_upd.mkt_order_balance,
        );*/

        // Deposit lamports for settlement log space
        if !inp_preview {
            let mut user_lamports = ctx.accounts.user.lamports();
            user_lamports = user_lamports.checked_sub(market.log_fee).ok_or(error!(ErrorCode::Overflow))?;
            **ctx.accounts.user.lamports.borrow_mut() = user_lamports;
            let mut market_lamports = state_upd.to_account_info().lamports();
            market_lamports = market_lamports.checked_add(market.log_fee).ok_or(error!(ErrorCode::Overflow))?;
            **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;
            state_upd.log_deposit_balance = state_upd.log_deposit_balance.checked_add(market.log_fee).ok_or(error!(ErrorCode::Overflow))?;
        }

        // Send tokens to the vault
        let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
        perform_transfer(ctx.remaining_accounts, mint_type, 0, inp_quantity, inp_preview,
            &ctx.accounts.user_mkt_token.to_account_info(),  // From
            &ctx.accounts.mkt_vault.to_account_info(),       // To
            &ctx.accounts.user.to_account_info(),            // Auth
            &ctx.accounts.spl_token_prog.to_account_info(),  // SPL Token Program
        )?;

        if tokens_filled > 0 {
            tokens_received = tokens_received.checked_sub(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
            result.set_tokens_received(tokens_received);
            if !inp_preview {
                // Withdraw tokens from the vault
                state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_sub(tokens_received).ok_or(error!(ErrorCode::Overflow))?;
                state_upd.prc_order_balance = state_upd.prc_order_balance.checked_sub(tokens_received).ok_or(error!(ErrorCode::Overflow))?;

                // Apply fees
                state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
                state_upd.prc_fees_balance = state_upd.prc_fees_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;

                //msg!("Atellix: Pricing Token Vault Withdraw: {}", tokens_received.to_string());
                /*msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
                    state_upd.prc_vault_balance,
                    state_upd.prc_order_balance,
                );*/

                let seeds = &[
                    market.to_account_info().key.as_ref(),
                    &[market.agent_nonce],
                ];
                let signer = &[&seeds[..]];
                let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, tokens_received,
                    &ctx.accounts.prc_vault.to_account_info(),          // From
                    &ctx.accounts.user_prc_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
            }
        }
        if *acc_result.key != *acc_user.key {
            store_struct::<TradeResult>(&result, acc_result)?;
        }

        if !inp_preview {
            msg!("atellix-log");
            emit!(OrderEvent {
                event_type: 295320270387787716737004386297471454892, // solana/program/aqua-dex/limit_ask/order
                action_id: state_upd.action_counter,
                market: market.key(),
                user: acc_user.key(),
                market_token: ctx.accounts.user_mkt_token.key(),
                pricing_token: ctx.accounts.user_prc_token.key(),
                order_id: result.order_id,
                order_side: Side::Ask as u8,
                filled: tokens_remaining == 0,
                tokens_received: result.tokens_received,
                tokens_sent: result.tokens_sent,
                tokens_fee: result.tokens_fee,
                posted: result.posted_quantity > 0,
                posted_quantity: result.posted_quantity,
                order_price: inp_price,
                order_quantity: inp_quantity,
                expires: expiry,
            });
        }

        Ok(result)
    }

    pub fn market_bid<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, OrderContext<'info>>,
        inp_by_quantity: bool,  // Fill by quantity (otherwise price)
        inp_quantity: u64,      // Fill until quantity
        inp_net_price: u64,     // Fill until net price is reached
        inp_fill: bool,         // Require order to be filled completely
        inp_preview: bool,      // Preview mode
        inp_rollover: bool,     // Perform settlement log rollover
    ) -> anchor_lang::Result<TradeResult> {
        if inp_by_quantity {
            require!(inp_quantity > 0, ErrorCode::InvalidParameters);
        } else {
            require!(inp_net_price > 0, ErrorCode::InvalidParameters);
        }
        let clock = Clock::get()?;
        let clock_ts = clock.unix_timestamp;

        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_user = &ctx.accounts.user.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        if !market.active {
            msg!("Market closed");
            return Err(ErrorCode::MarketClosed.into());
        }
        if inp_by_quantity {
            require!(inp_quantity >= market.min_quantity, ErrorCode::QuantityBelowMinimum);
        }

        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let s1 = verify_matching_accounts(&market_state.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market_state.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into());
        }

        // Append a settlement log account
        let state_upd = &mut ctx.accounts.state;
        if inp_rollover && !inp_preview {
            if !state_upd.log_rollover {
                // Another market participant already appended a new log account (please retry transaction)
                msg!("Please update market data and retry");
                return Err(ErrorCode::RetrySettlementAccount.into());
            }
            let av = ctx.remaining_accounts;
            let new_settlement_log = av.get(0).unwrap();
            let market_pk: Pubkey = market.key();
            log_rollover(state_upd, market_pk, acc_settle2, new_settlement_log)?;
            log_reimburse(market, state_upd, acc_user)?;
            let mut market_lamports = state_upd.to_account_info().lamports();
            market_lamports = market_lamports.checked_sub(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;
            **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;
        }

        msg!("Atellix: Market Bid: By Qty: {} Quantity: {} Net Price: {}", inp_by_quantity.to_string(), inp_quantity.to_string(), inp_net_price.to_string());

        let mkt_decimal_base: u64 = 10;
        let mkt_decimal_factor: u64 = mkt_decimal_base.pow(market.mkt_decimals as u32);

        if !inp_preview {
            state_upd.action_counter = state_upd.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
        }

        let orderbook_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let ob = SlabPageAlloc::new(orderbook_data);

        // Check if order can be filled
        let mut price_to_fill: u64 = inp_net_price;
        let mut tokens_to_fill: u64 = inp_quantity;
        let mut tokens_filled: u64 = 0;
        let mut tokens_paid: u64 = 0;
        let mut tokens_fee: u64 = 0;
        let mut expired_orders = Vec::new();
        let acc_trade_log = &ctx.accounts.trade_log.to_account_info();
        verify_matching_accounts(&market.trade_log, &acc_trade_log.key, Some(String::from("Invalid trade log")))?;
        let trade_data: &mut[u8] = &mut acc_trade_log.try_borrow_mut_data()?;
        let tlog = SlabPageAlloc::new(trade_data);
        loop {
            let node_res = map_predicate_min(ob, DT::AskOrder, |sl, leaf|
                valid_order(OrderDT::AskOrder, leaf, acc_user.key, sl, &mut expired_orders, clock_ts)
            );
            if node_res.is_none() {
                msg!("Atellix: No Match");
                break;
            }
            let posted_node = node_res.unwrap();
            let posted_order = ob.index::<Order>(OrderDT::AskOrder as u16, posted_node.slot() as usize);
            let posted_qty = posted_order.amount;
            let posted_price = Order::price(posted_node.key());
            msg!("Atellix: Matched Ask [{}] {} @ {}", posted_node.slot().to_string(), posted_qty.to_string(), posted_price.to_string());
            // Fill order
            if inp_by_quantity {
                // Fill until quantity
                if posted_qty == tokens_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_part.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            97879353062914658353780090028087623355, // solana/program/aqua-dex/market_bid/match/quantity/exact
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::AskOrder, posted_node.key())?;
                        Order::free_index(ob, DT::AskOrder, posted_node.slot())?;
                        state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    }
                    break;
                } else if posted_qty < tokens_to_fill {   // Match the entire order and continue
                    tokens_to_fill = tokens_to_fill.checked_sub(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(posted_qty, posted_price, mkt_decimal_factor)?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", posted_qty.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            98887148454935384202006639804150096432, // solana/program/aqua-dex/market_bid/match/quantity/entire
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::AskOrder, posted_node.key())?;
                        Order::free_index(ob, DT::AskOrder, posted_node.slot())?;
                        state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    }
                } else if posted_qty > tokens_to_fill {   // Match part of the order
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_to_fill.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            241528249049192735796332143519520355761, // solana/program/aqua-dex/market_bid/match/quantity/partial
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            false,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        let new_amount = posted_qty.checked_sub(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        ob.index_mut::<Order>(OrderDT::AskOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    }
                    break;
                }
            } else {
                // Fill until price
                let posted_part = scale_price(posted_qty, posted_price, mkt_decimal_factor)?;
                if posted_part == price_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_paid = tokens_paid.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, posted_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_filled.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            331852354717548342008417076114136032746, // solana/program/aqua-dex/market_bid/match/net_price/exact
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::AskOrder, posted_node.key())?;
                        Order::free_index(ob, DT::AskOrder, posted_node.slot())?;
                        state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, posted_part)?;
                    }
                    break;
                } else if posted_part < price_to_fill {   // Match the entire order and continue
                    price_to_fill = price_to_fill.checked_sub(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_paid = tokens_paid.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, posted_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", posted_qty.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            30314321964017162377189412309266042294, // solana/program/aqua-dex/market_bid/match/net_price/entire
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::AskOrder, posted_node.key())?;
                        Order::free_index(ob, DT::AskOrder, posted_node.slot())?;
                        state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, posted_part)?;
                    }
                } else if posted_part > price_to_fill {   // Match part of the order
                    // Calculate filled tokens
                    let fill_amount = fill_quantity(price_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_filled = tokens_filled.checked_add(fill_amount).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_paid = tokens_paid.checked_add(price_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, price_to_fill)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", fill_amount.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            237563056127520713232024370460619306548, // solana/program/aqua-dex/market_bid/match/net_price/partial
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            false,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Bid as u8,
                            fill_amount,
                            posted_price,
                            clock_ts
                        )?;
                        let new_amount = posted_qty.checked_sub(fill_amount).ok_or(error!(ErrorCode::Overflow))?;
                        ob.index_mut::<Order>(OrderDT::AskOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(price_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(price_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, price_to_fill)?;
                    }
                    break;
                }
            }
        }
        msg!("Atellix: Fee: {}", tokens_fee.to_string());

        let mut expired_count: u32 = 0;
        if expired_orders.len() > 0 && !inp_preview {
            loop {
                if expired_orders.len() == 0 || expired_count == MAX_EXPIRATIONS {
                    break;
                }
                let expired_id: u128 = expired_orders.pop().unwrap();
                let expire_leaf = map_get(ob, DT::AskOrder, expired_id).unwrap();
                let expire_order = *ob.index::<Order>(OrderDT::AskOrder as u16, expire_leaf.slot() as usize);
                let expire_amount: u64 = expire_order.amount();
                msg!("Atellix: Expired Order[{}] - Owner: {} {} @ {}",
                    expire_leaf.slot().to_string(),
                    expire_leaf.owner().to_string(),
                    expire_order.amount().to_string(),
                    Order::price(expire_leaf.key()).to_string(),
                );
                msg!("atellix-log");
                emit!(ExpireEvent {
                    event_type: 16332991664789055110548783525139174482, // solana/program/aqua-dex/expire_event
                    action_id: state_upd.action_counter,
                    market: market.key(),
                    owner: expire_leaf.owner(),
                    order_side: Side::Ask as u8,
                    order_id: expired_id,
                    price: Order::price(expire_leaf.key()),
                    quantity: expire_amount,
                    tokens: expire_amount,
                });
                log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &expire_leaf.owner(), true, expire_amount)?; // No multiply for Ask order
                map_remove(ob, DT::AskOrder, expire_leaf.key())?;
                Order::free_index(ob, DT::AskOrder, expire_leaf.slot())?;
                state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                expired_count = expired_count + 1;
            }
        }

        let mut result = TradeResult { tokens_received: tokens_filled, posted_quantity: 0, tokens_sent: 0, tokens_fee: tokens_fee, order_id: 0 };

        if inp_fill {
            if inp_by_quantity {
                if tokens_filled != inp_quantity {
                    msg!("Order not filled");
                    return Err(ErrorCode::OrderNotFilled.into());
                }
            } else {
                if tokens_paid != inp_net_price {
                    msg!("Order not filled");
                    return Err(ErrorCode::OrderNotFilled.into());
                }
            }
        }

        // Apply fees
        tokens_paid = tokens_paid.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
        if !inp_preview {
            state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.prc_fees_balance = state_upd.prc_fees_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
        }

        /*msg!("Atellix: Pricing Token Vault Deposit: {}", total_cost.to_string());
        msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
            state_upd.prc_vault_balance,
            state_upd.prc_order_balance,
        );*/

        // Send tokens to the vault
        let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
        perform_transfer(ctx.remaining_accounts, mint_type, 0, tokens_paid, inp_preview,
            &ctx.accounts.user_prc_token.to_account_info(),  // From
            &ctx.accounts.prc_vault.to_account_info(),       // To
            &ctx.accounts.user.to_account_info(),            // Auth
            &ctx.accounts.spl_token_prog.to_account_info(),  // SPL Token Program
        )?;
        result.set_tokens_sent(tokens_paid);

        if tokens_filled > 0 && !inp_preview {
            // Withdraw tokens from the vault
            state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_sub(tokens_filled).ok_or(error!(ErrorCode::Overflow))?;
            state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_sub(tokens_filled).ok_or(error!(ErrorCode::Overflow))?;

            /*msg!("Atellix: Market Token Vault Withdraw: {}", tokens_filled.to_string());
            msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
                state_upd.mkt_vault_balance,
                state_upd.mkt_order_balance,
            );*/

            let seeds = &[market.to_account_info().key.as_ref(), &[market.agent_nonce]];
            let signer = &[&seeds[..]];
            let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
            perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, tokens_filled,
                &ctx.accounts.mkt_vault.to_account_info(),          // From
                &ctx.accounts.user_mkt_token.to_account_info(),     // To
                &ctx.accounts.agent.to_account_info(),              // Auth
                &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
            )?;
        }
        if *acc_result.key != *acc_user.key {
            store_struct::<TradeResult>(&result, acc_result)?;
        }
        let was_filled: bool = if inp_by_quantity {
            tokens_filled == inp_quantity
        } else {
            tokens_paid == inp_net_price
        };

        if !inp_preview {
            msg!("atellix-log");
            emit!(OrderEvent {
                event_type: 151919600483167167737000078670308605753, // solana/program/aqua-dex/market_bid/order
                action_id: state_upd.action_counter,
                market: market.key(),
                user: acc_user.key(),
                market_token: ctx.accounts.user_mkt_token.key(),
                pricing_token: ctx.accounts.user_prc_token.key(),
                order_id: 0,
                order_side: Side::Bid as u8,
                filled: was_filled,
                tokens_received: result.tokens_received,
                tokens_sent: result.tokens_sent,
                tokens_fee: tokens_fee,
                posted: false,
                posted_quantity: 0,
                order_price: inp_net_price,
                order_quantity: inp_quantity,
                expires: 0,
            });
        }

        Ok(result)
    }

    pub fn market_ask<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, OrderContext<'info>>,
        inp_by_quantity: bool,  // Fill by quantity (otherwise price)
        inp_quantity: u64,      // Fill until quantity
        inp_net_price: u64,     // Fill until net price is reached
        inp_fill: bool,         // Require order to be filled completely
        inp_preview: bool,      // Preview mode
        inp_rollover: bool,     // Perform settlement log rollover
    ) -> anchor_lang::Result<TradeResult> {
        if inp_by_quantity {
            require!(inp_quantity > 0, ErrorCode::InvalidParameters);
        } else {
            require!(inp_net_price > 0, ErrorCode::InvalidParameters);
        }
        let clock = Clock::get()?;
        let clock_ts = clock.unix_timestamp;

        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_user = &ctx.accounts.user.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        if !market.active {
            msg!("Market closed");
            return Err(ErrorCode::MarketClosed.into());
        }
        if inp_by_quantity {
            require!(inp_quantity >= market.min_quantity, ErrorCode::QuantityBelowMinimum);
        }

        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let s1 = verify_matching_accounts(&market_state.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market_state.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into()); 
        }

        // Append a settlement log account
        let state_upd = &mut ctx.accounts.state;
        if inp_rollover && !inp_preview {
            if !state_upd.log_rollover {
                // Another market participant already appended a new log account (please retry transaction)
                msg!("Please update market data and retry");
                return Err(ErrorCode::RetrySettlementAccount.into());
            }
            let av = ctx.remaining_accounts;
            let new_settlement_log = av.get(0).unwrap();
            let market_pk: Pubkey = market.key();
            log_rollover(state_upd, market_pk, acc_settle2, new_settlement_log)?;
            log_reimburse(market, state_upd, acc_user)?;
            let mut market_lamports = state_upd.to_account_info().lamports();
            market_lamports = market_lamports.checked_sub(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;
            **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;
        }

        msg!("Atellix: Market Ask: By Qty: {} Quantity: {} Net Price: {}", inp_by_quantity.to_string(), inp_quantity.to_string(), inp_net_price.to_string());

        let state_upd = &mut ctx.accounts.state;
        if !inp_preview {
            state_upd.action_counter = state_upd.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
        }

        let orderbook_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let ob = SlabPageAlloc::new(orderbook_data);

        let mkt_decimal_base: u64 = 10;
        let mkt_decimal_factor: u64 = mkt_decimal_base.pow(market.mkt_decimals as u32);

        // Check if order can be filled
        let mut price_to_fill: u64 = inp_net_price;
        let mut tokens_to_fill: u64 = inp_quantity;
        let mut tokens_filled: u64 = 0;
        let mut tokens_received: u64 = 0;
        let mut tokens_fee: u64 = 0;
        let mut expired_orders = Vec::new();
        let acc_trade_log = &ctx.accounts.trade_log.to_account_info();
        verify_matching_accounts(&market.trade_log, &acc_trade_log.key, Some(String::from("Invalid trade log")))?;
        let trade_data: &mut[u8] = &mut acc_trade_log.try_borrow_mut_data()?;
        let tlog = SlabPageAlloc::new(trade_data);
        loop {
            let node_res = map_predicate_max(ob, DT::BidOrder, |sl, leaf|
                valid_order(OrderDT::BidOrder, leaf, acc_user.key, sl, &mut expired_orders, clock_ts)
            );
            if node_res.is_none() {
                msg!("Atellix: No Match");
                break;
            }
            let posted_node = node_res.unwrap();
            let posted_order = ob.index::<Order>(OrderDT::BidOrder as u16, posted_node.slot() as usize);
            let posted_qty = posted_order.amount;
            let posted_price = Order::price(posted_node.key());
            msg!("Atellix: Matched Bid [{}] {} @ {}", posted_node.slot().to_string(), posted_qty.to_string(), posted_price.to_string());
            if inp_by_quantity {
                // Fill order by quantity
                if posted_qty == tokens_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_part.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            176535012143782409593813433848999612355, // solana/program/aqua-dex/market_ask/match/quantity/exact
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::BidOrder, posted_node.key())?;
                        Order::free_index(ob, DT::BidOrder, posted_node.slot())?;
                        state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, tokens_to_fill)?;
                    }
                    break;
                } else if posted_qty < tokens_to_fill {   // Match the entire order and continue
                    tokens_to_fill = tokens_to_fill.checked_sub(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(posted_qty, posted_price, mkt_decimal_factor)?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", posted_qty.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            277111811349020061708541382826182055538, // solana/program/aqua-dex/market_ask/match/quantity/entire
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::BidOrder, posted_node.key())?;
                        Order::free_index(ob, DT::BidOrder, posted_node.slot())?;
                        state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, posted_qty)?;
                    }
                } else if posted_qty > tokens_to_fill {   // Match part of the order
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    let tokens_part = scale_price(tokens_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, tokens_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", tokens_to_fill.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            338129135642557935308794285239529753670, // solana/program/aqua-dex/market_ask/match/quantity/partial
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            false,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            tokens_to_fill,
                            posted_price,
                            clock_ts
                        )?;
                        let new_amount = posted_qty.checked_sub(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        ob.index_mut::<Order>(OrderDT::BidOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                        state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(tokens_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, tokens_to_fill)?;
                    }
                    break;
                }
            } else {
                // Fill until price
                let posted_part = scale_price(posted_qty, posted_price, mkt_decimal_factor)?;
                if posted_part == price_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_received = tokens_received.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, posted_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", posted_qty.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            38185514874311817824997288786026180382, // solana/program/aqua-dex/market_ask/match/net_price/exact
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::BidOrder, posted_node.key())?;
                        Order::free_index(ob, DT::BidOrder, posted_node.slot())?;
                        state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, posted_qty)?;
                    }
                    break;
                } else if posted_part < price_to_fill {   // Match the entire order and continue
                    price_to_fill = price_to_fill.checked_sub(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_received = tokens_received.checked_add(posted_part).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, posted_part)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", posted_qty.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            48115079441646063920817461881527222742, // solana/program/aqua-dex/market_ask/match/net_price/entire
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            true,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            posted_qty,
                            posted_price,
                            clock_ts
                        )?;
                        map_remove(ob, DT::BidOrder, posted_node.key())?;
                        Order::free_index(ob, DT::BidOrder, posted_node.slot())?;
                        state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(posted_qty).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, posted_qty)?;
                    }
                } else if posted_part > price_to_fill {   // Match part of the order
                    let fill_amount = fill_quantity(price_to_fill, posted_price, mkt_decimal_factor)?;
                    tokens_filled = tokens_filled.checked_add(fill_amount).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_received = tokens_received.checked_add(price_to_fill).ok_or(error!(ErrorCode::Overflow))?;
                    tokens_fee = tokens_fee.checked_add(calculate_fee(market.taker_fee, price_to_fill)?).ok_or(error!(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - {} @ {}", fill_amount.to_string(), posted_price.to_string());
                    if !inp_preview {
                        log_trade(tlog,
                            338446361041777477888718125403430758950, // solana/program/aqua-dex/market_ask/match/net_price/partial
                            state_upd.action_counter,
                            &market.key(),
                            posted_node.key(),
                            false,
                            &posted_node.owner(),
                            &acc_user.key(),
                            Side::Ask as u8,
                            fill_amount,
                            posted_price,
                            clock_ts
                        )?;
                        let new_amount = posted_qty.checked_sub(fill_amount).ok_or(error!(ErrorCode::Overflow))?;
                        ob.index_mut::<Order>(OrderDT::BidOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                        state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(fill_amount).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(fill_amount).ok_or(error!(ErrorCode::Overflow))?;
                        state_upd.last_price = posted_price;
                        state_upd.last_ts = clock_ts;
                        log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, fill_amount)?;
                    }
                    break;
                }
            }
        }

        msg!("Atellix: Fee: {}", tokens_fee.to_string());

        let mut expired_count: u32 = 0;
        if expired_orders.len() > 0 && !inp_preview {
            loop {
                if expired_orders.len() == 0 || expired_count == MAX_EXPIRATIONS {
                    break;
                }
                let expired_id: u128 = expired_orders.pop().unwrap();
                let expire_leaf = map_get(ob, DT::BidOrder, expired_id).unwrap();
                let expire_order = *ob.index::<Order>(OrderDT::BidOrder as u16, expire_leaf.slot() as usize);
                let expire_amount: u64 = expire_order.amount();
                msg!("Atellix: Expired Order[{}] - Owner: {} {} @ {}",
                    expire_leaf.slot().to_string(),
                    expire_leaf.owner().to_string(),
                    expire_order.amount().to_string(),
                    Order::price(expire_leaf.key()).to_string(),
                );
                let expire_price = Order::price(expire_leaf.key());
                let expire_total = scale_price(expire_amount, expire_price, mkt_decimal_factor)?;
                msg!("atellix-log");
                emit!(ExpireEvent {
                    event_type: 16332991664789055110548783525139174482, // solana/program/aqua-dex/expire_event
                    action_id: state_upd.action_counter,
                    market: market.key(),
                    owner: expire_leaf.owner(),
                    order_side: Side::Bid as u8,
                    order_id: expired_id,
                    price: Order::price(expire_leaf.key()),
                    quantity: expire_amount,
                    tokens: expire_total,
                });
                log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &expire_leaf.owner(), false, expire_total)?; // Total calculated
                map_remove(ob, DT::BidOrder, expire_leaf.key())?;
                Order::free_index(ob, DT::BidOrder, expire_leaf.slot())?;
                state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                expired_count = expired_count + 1;
            }
        }

        let mut result = TradeResult { tokens_received: 0, posted_quantity: 0, tokens_sent: tokens_filled, tokens_fee: tokens_fee, order_id: 0 };

        if inp_fill {
            if inp_by_quantity {
                if tokens_filled != inp_quantity {
                    msg!("Order not filled");
                    return Err(ErrorCode::OrderNotFilled.into());
                }
            } else {
                if tokens_received != inp_net_price {
                    msg!("Order not filled");
                    return Err(ErrorCode::OrderNotFilled.into());
                }
            }
        }

        /*msg!("Atellix: Market Token Vault Deposit: {}", inp_quantity.to_string());
        msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
            state_upd.mkt_vault_balance,
            state_upd.mkt_order_balance,
        );*/

        // Send tokens to the vault
        let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
        perform_transfer(ctx.remaining_accounts, mint_type, 0, tokens_filled, inp_preview,
            &ctx.accounts.user_mkt_token.to_account_info(),  // From
            &ctx.accounts.mkt_vault.to_account_info(),       // To
            &ctx.accounts.user.to_account_info(),            // Auth
            &ctx.accounts.spl_token_prog.to_account_info(),  // SPL Token Program
        )?;

        if tokens_filled > 0 {
            tokens_received = tokens_received.checked_sub(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
            result.set_tokens_received(tokens_received);
            if !inp_preview {
                // Withdraw tokens from the vault
                state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_sub(tokens_received).ok_or(error!(ErrorCode::Overflow))?;
                state_upd.prc_order_balance = state_upd.prc_order_balance.checked_sub(tokens_received).ok_or(error!(ErrorCode::Overflow))?;

                // Apply fees
                state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;
                state_upd.prc_fees_balance = state_upd.prc_fees_balance.checked_add(tokens_fee).ok_or(error!(ErrorCode::Overflow))?;

                //msg!("Atellix: Pricing Token Vault Withdraw: {}", tokens_received.to_string());
                /*msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
                    state_upd.prc_vault_balance,
                    state_upd.prc_order_balance,
                );*/

                let seeds = &[
                    market.to_account_info().key.as_ref(),
                    &[market.agent_nonce],
                ];
                let signer = &[&seeds[..]];
                let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, tokens_received,
                    &ctx.accounts.prc_vault.to_account_info(),          // From
                    &ctx.accounts.user_prc_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
            }
        }
        if *acc_result.key != *acc_user.key {
            store_struct::<TradeResult>(&result, acc_result)?;
        }
        let was_filled: bool = if inp_by_quantity {
            tokens_filled == inp_quantity
        } else {
            tokens_received == inp_net_price
        };

        if !inp_preview {
            msg!("atellix-log");
            emit!(OrderEvent {
                event_type: 116790064293172396704069821733243480358, // solana/program/aqua-dex/market_ask/order
                action_id: state_upd.action_counter,
                market: market.key(),
                user: acc_user.key(),
                market_token: ctx.accounts.user_mkt_token.key(),
                pricing_token: ctx.accounts.user_prc_token.key(),
                order_id: result.order_id,
                order_side: Side::Ask as u8,
                filled: was_filled,
                tokens_received: result.tokens_received,
                tokens_sent: result.tokens_sent,
                tokens_fee: result.tokens_fee,
                posted: result.posted_quantity > 0,
                posted_quantity: result.posted_quantity,
                order_price: inp_net_price,
                order_quantity: inp_quantity,
                expires: 0,
            });
        }

        Ok(result)
    }

    pub fn cancel_order<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, CancelOrder<'info>>,
        inp_side: u8,               // 0 - Bid, 1 - Ask
        inp_order_id: u128,
    ) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_owner = &ctx.accounts.owner.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let side = Side::try_from(inp_side).or(Err(error!(ErrorCode::InvalidParameters)))?;
        let order_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let sl = SlabPageAlloc::new(order_data);
        let order_type = match side {
            Side::Bid => DT::BidOrder,
            Side::Ask => DT::AskOrder,
        };
        let item = map_get(sl, order_type, inp_order_id);
        if item.is_none() {
            msg!("Order not found");
            return Err(ErrorCode::OrderNotFound.into());
        }
        let leaf = item.unwrap();
        if leaf.owner() != *acc_owner.key {
            msg!("Order not owned by user");
            return Err(ErrorCode::AccessDenied.into());
        }
        let order = sl.index::<Order>(index_datatype(order_type), leaf.slot() as usize);
        let state = &mut ctx.accounts.state;
        state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
        if side == Side::Bid {
            state.active_bid = state.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
        } else if side == Side::Ask {
            state.active_ask = state.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
        }

        let mut result = WithdrawResult { mkt_tokens: 0, prc_tokens: 0 };
        let order_id = leaf.key();
        let order_price = Order::price(order_id);
        let order_qty = order.amount();
        let tokens_out = match side {
            Side::Bid => {
                let mkt_decimal_base: u64 = 10;
                let mkt_decimal_factor: u64 = mkt_decimal_base.pow(market.mkt_decimals as u32);
                let total = scale_price(order_qty, order_price, mkt_decimal_factor)?;
                result.set_prc_tokens(total);
                state.prc_vault_balance = state.prc_vault_balance.checked_sub(total).ok_or(error!(ErrorCode::Overflow))?;
                state.prc_order_balance = state.prc_order_balance.checked_sub(total).ok_or(error!(ErrorCode::Overflow))?;
                total
            },
            Side::Ask => {
                let total = order.amount();
                result.set_mkt_tokens(total);
                state.mkt_vault_balance = state.mkt_vault_balance.checked_sub(total).ok_or(error!(ErrorCode::Overflow))?;
                state.mkt_order_balance = state.mkt_order_balance.checked_sub(total).ok_or(error!(ErrorCode::Overflow))?;
                total
            }
        };
        map_remove(sl, order_type, leaf.key())?;
        Order::free_index(sl, order_type, leaf.slot())?;

        // Rebate to the user for settlement log space
        state.log_deposit_balance = state.log_deposit_balance.checked_sub(market.log_rebate).ok_or(error!(ErrorCode::Overflow))?;
        let mut market_lamports = state.to_account_info().lamports();
        market_lamports = market_lamports.checked_sub(market.log_rebate).ok_or(error!(ErrorCode::Overflow))?;
        **state.to_account_info().lamports.borrow_mut() = market_lamports;
        let mut user_lamports = ctx.accounts.owner.lamports();
        user_lamports = user_lamports.checked_add(market.log_rebate).ok_or(error!(ErrorCode::Overflow))?;
        **ctx.accounts.owner.lamports.borrow_mut() = user_lamports;

        let seeds = &[ctx.accounts.market.to_account_info().key.as_ref(), &[market.agent_nonce]];
        let signer = &[&seeds[..]];
        if side == Side::Bid {
            let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
            perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, tokens_out,
                &ctx.accounts.prc_vault.to_account_info(),          // From
                &ctx.accounts.user_prc_token.to_account_info(),     // To
                &ctx.accounts.agent.to_account_info(),              // Auth
                &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
            )?;
        } else if side == Side::Ask {
            let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
            perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, tokens_out,
                &ctx.accounts.mkt_vault.to_account_info(),          // From
                &ctx.accounts.user_mkt_token.to_account_info(),     // To
                &ctx.accounts.agent.to_account_info(),              // Auth
                &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
            )?;
        }
        if *acc_result.key != *acc_owner.key {
            store_struct::<WithdrawResult>(&result, acc_result)?;
        }

        msg!("atellix-log");
        emit!(CancelEvent {
            event_type: 80941766873992229586089855487021729071, // solana/program/aqua-dex/cancel_order
            action_id: state.action_counter,
            market: ctx.accounts.market.key(),
            owner: acc_owner.key(),
            user: acc_owner.key(),
            market_token: ctx.accounts.user_mkt_token.key(),
            pricing_token: ctx.accounts.user_prc_token.key(),
            manager: false,
            order_side: side as u8,
            order_id: order_id,
            order_price: order_price,
            order_quantity: order_qty,
            token_withdrawn: tokens_out,
        });

        Ok(())
    }

    // Withdraw tokens from the settlement vault
    pub fn withdraw<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, Withdraw<'info>>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_owner = &ctx.accounts.owner.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_settle = &ctx.accounts.settle.to_account_info();
        let acc_settle_prev = &ctx.accounts.settle_prev.to_account_info();
        let acc_settle_next = &ctx.accounts.settle_next.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        // Verify 
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;

        state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;

        let mut market_tokens: u64 = 0;
        let mut pricing_tokens: u64 = 0;
        let owner_id: u128 = CritMap::bytes_hash(acc_owner.key.as_ref());
        let log_data: &mut[u8] = &mut acc_settle.try_borrow_mut_data()?;
        let (header, page_table) = mut_array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
        let settle_header: &mut [AccountsHeader] = cast_slice_mut(header);
        verify_matching_accounts(&settle_header[0].market, &market.key(), Some(String::from("Invalid market")))?;
        let close_log: bool = settle_header[0].items == 1 && settle_header[0].prev != Pubkey::default() && settle_header[0].next != Pubkey::default();
        let sl = SlabPageAlloc::new(page_table);
        let has_item = map_get(sl, DT::Account, owner_id);
        if has_item.is_some() {
            let log_node = has_item.unwrap();
            let log_entry = sl.index::<AccountEntry>(SettleDT::Account as u16, log_node.slot() as usize);
            let seeds = &[ctx.accounts.market.to_account_info().key.as_ref(), &[market.agent_nonce]];
            let signer = &[&seeds[..]];
            let mut result = WithdrawResult { mkt_tokens: 0, prc_tokens: 0 };
            if log_entry.mkt_token_balance() > 0 {
                market_tokens = log_entry.mkt_token_balance();
                result.set_mkt_tokens(market_tokens);
                let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, log_entry.mkt_token_balance(),
                    &ctx.accounts.mkt_vault.to_account_info(),          // From
                    &ctx.accounts.user_mkt_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                state.mkt_log_balance = state.mkt_log_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.mkt_vault_balance = state.mkt_vault_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            if log_entry.prc_token_balance() > 0 {
                pricing_tokens = log_entry.prc_token_balance();
                result.set_prc_tokens(pricing_tokens);
                let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, log_entry.prc_token_balance(),
                    &ctx.accounts.prc_vault.to_account_info(),          // From
                    &ctx.accounts.user_prc_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                state.prc_log_balance = state.prc_log_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.prc_vault_balance = state.prc_vault_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            // Remove log entry
            settle_header[0].items = settle_header[0].items.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
            map_remove(sl, DT::Account, log_node.key())?;
            AccountEntry::free_index(sl, DT::Account, log_node.slot())?;

            // Rebate to the user for settlement log space
            state.log_deposit_balance = state.log_deposit_balance.checked_sub(market.log_rebate).ok_or(error!(ErrorCode::Overflow))?;
            let mut market_lamports = state.to_account_info().lamports();
            market_lamports = market_lamports.checked_sub(market.log_rebate).ok_or(error!(ErrorCode::Overflow))?;
            let mut user_lamports = ctx.accounts.owner.lamports();
            user_lamports = user_lamports.checked_add(market.log_rebate).ok_or(error!(ErrorCode::Overflow))?;
            **ctx.accounts.owner.lamports.borrow_mut() = user_lamports;

            // Write result
            if *acc_result.key != ctx.accounts.owner.key() {
                store_struct::<WithdrawResult>(&result, acc_result)?;
            }

            // Close log if necessary
            if close_log {
                let log_lamports = log_close(state, acc_settle, acc_settle_prev, acc_settle_next)?;
                market_lamports = market_lamports.checked_add(log_lamports).ok_or(error!(ErrorCode::Overflow))?;
                state.log_deposit_balance = state.log_deposit_balance.checked_add(log_lamports).ok_or(error!(ErrorCode::Overflow))?;
            }
            **state.to_account_info().lamports.borrow_mut() = market_lamports;
        } else {
            msg!("Account not found");
            return Err(ErrorCode::AccountNotFound.into());
        }

        msg!("atellix-log");
        emit!(WithdrawEvent {
            event_type: 206836899720010235937021599972903459637, // solana/program/aqua-dex/withdraw
            action_id: state.action_counter,
            market: ctx.accounts.market.key(),
            owner: ctx.accounts.owner.key(),
            user: ctx.accounts.owner.key(),
            market_account: ctx.accounts.user_mkt_token.key(),
            pricing_account: ctx.accounts.user_prc_token.key(),
            manager: false,
            market_tokens: market_tokens,
            pricing_tokens: pricing_tokens,
        });

        Ok(())
    }

    pub fn expire_order<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ExpireOrder<'info>>,
        inp_side: u8,               // 0 - Bid, 1 - Ask
        inp_order_id: u128,
        inp_rollover: bool,
    ) -> anchor_lang::Result<()> {
        let clock = Clock::get()?;
        let clock_ts = clock.unix_timestamp;

        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_user = &ctx.accounts.user.to_account_info();
        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();

        let s1 = verify_matching_accounts(&market_state.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market_state.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into()); 
        }

        // Append a settlement log account
        let state_upd = &mut ctx.accounts.state;
        if inp_rollover {
            if !state_upd.log_rollover {
                // Another market participant already appended a new log account (please retry transaction)
                msg!("Please update market data and retry");
                return Err(ErrorCode::RetrySettlementAccount.into());
            }
            let av = ctx.remaining_accounts;
            let new_settlement_log = av.get(0).unwrap();
            let market_pk: Pubkey = market.key();
            log_rollover(state_upd, market_pk, acc_settle2, new_settlement_log)?;
            log_reimburse(market, state_upd, acc_user)?;
            let mut market_lamports = state_upd.to_account_info().lamports();
            market_lamports = market_lamports.checked_sub(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;
            **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;
        }

        let side = Side::try_from(inp_side).or(Err(error!(ErrorCode::InvalidParameters)))?;
        let order_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let sl = SlabPageAlloc::new(order_data);
        let order_type = match side {
            Side::Bid => DT::BidOrder,
            Side::Ask => DT::AskOrder,
        };
        let item = map_get(sl, order_type, inp_order_id);
        if item.is_none() {
            msg!("Order not found");
            return Err(ErrorCode::OrderNotFound.into());
        }
        let leaf = item.unwrap();
        let order = sl.index::<Order>(index_datatype(order_type), leaf.slot() as usize);
        let expired: bool = order.expiry != 0 && order.expiry >= clock_ts;      // Check expiry timestamp if needed
        if expired {
            state_upd.action_counter = state_upd.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
            let order_id = leaf.key();
            let order_owner: Pubkey = leaf.owner();
            let order_price = Order::price(order_id);
            let order_qty = order.amount();
            let tokens = match side {
                Side::Bid => {
                    let mkt_decimal_base: u64 = 10;
                    let mkt_decimal_factor: u64 = mkt_decimal_base.pow(market.mkt_decimals as u32);
                    let total = scale_price(order_qty, order_price, mkt_decimal_factor)?;
                    log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &order_owner, false, total)?;
                    state_upd.active_bid = state_upd.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                    total
                },
                Side::Ask => {
                    let total = order.amount();
                    log_settlement(&market.key(), state_upd, acc_settle1, acc_settle2, &order_owner, true, total)?;
                    state_upd.active_ask = state_upd.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                    total
                }
            };
            map_remove(sl, order_type, leaf.key())?;
            Order::free_index(sl, order_type, leaf.slot())?;

            msg!("atellix-log");
            emit!(ExpireEvent {
                event_type: 16332991664789055110548783525139174482, // solana/program/aqua-dex/expire_event
                action_id: state_upd.action_counter,
                market: market.key(),
                owner: leaf.owner(),
                order_side: Side::Bid as u8,
                order_id: order_id,
                price: order_price,
                quantity: order_qty,
                tokens: tokens,
            });
        }

        Ok(())
    }

    pub fn manager_cancel_order<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ManagerCancelOrder<'info>>,
        inp_side: u8,               // 0 - Bid, 1 - Ask
        inp_order_id: u128,
        inp_rollover: bool,
    ) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_manager = &ctx.accounts.manager.to_account_info();
        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        if !market.manager_cancel {
            msg!("Manager order cancellation disabled");
            return Err(ErrorCode::AccessDenied.into());
        }
        if market.manager != *acc_manager.key {
            msg!("Not manager");
            return Err(ErrorCode::AccessDenied.into());
        }
        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let s1 = verify_matching_accounts(&market_state.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market_state.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into()); 
        }

        // Append a settlement log account
        let state_upd = &mut ctx.accounts.state;
        if inp_rollover {
            if !state_upd.log_rollover {
                // Another market participant already appended a new log account (please retry transaction)
                msg!("Please update market data and retry");
                return Err(ErrorCode::RetrySettlementAccount.into());
            }
            let av = ctx.remaining_accounts;
            let new_settlement_log = av.get(0).unwrap();
            let market_pk: Pubkey = market.key();
            log_rollover(state_upd, market_pk, acc_settle2, new_settlement_log)?;
            // Manager is not reimbursed for settlement log rollover
        }

        let side = Side::try_from(inp_side).or(Err(error!(ErrorCode::InvalidParameters)))?;
        let order_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let sl = SlabPageAlloc::new(order_data);
        let order_type = match side {
            Side::Bid => DT::BidOrder,
            Side::Ask => DT::AskOrder,
        };
        let item = map_get(sl, order_type, inp_order_id);
        if item.is_none() {
            msg!("Order not found");
            return Err(ErrorCode::OrderNotFound.into());
        }
        let leaf = item.unwrap();
        let order = sl.index::<Order>(index_datatype(order_type), leaf.slot() as usize);
        let state = &mut ctx.accounts.state;
        state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
        let mut result = WithdrawResult { mkt_tokens: 0, prc_tokens: 0 };
        let order_id = leaf.key();
        let order_owner: Pubkey = leaf.owner();
        let order_price = Order::price(order_id);
        let order_qty = order.amount();
        let tokens_out = match side {
            Side::Bid => {
                let mkt_decimal_base: u64 = 10;
                let mkt_decimal_factor: u64 = mkt_decimal_base.pow(market.mkt_decimals as u32);
                let total = scale_price(order_qty, order_price, mkt_decimal_factor)?;
                result.set_prc_tokens(total);
                log_settlement(&market.key(), state, acc_settle1, acc_settle2, &order_owner, false, total)?;
                state.active_bid = state.active_bid.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                total
            },
            Side::Ask => {
                let total = order.amount();
                result.set_mkt_tokens(total);
                log_settlement(&market.key(), state, acc_settle1, acc_settle2, &order_owner, true, total)?;
                state.active_ask = state.active_ask.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
                total
            }
        };
        map_remove(sl, order_type, leaf.key())?;
        Order::free_index(sl, order_type, leaf.slot())?;

        if *acc_result.key != *acc_manager.key {
            store_struct::<WithdrawResult>(&result, acc_result)?;
        }

        msg!("atellix-log");
        emit!(CancelEvent {
            event_type: 149668793492806786255339444097076784738, // solana/program/aqua-dex/manager_cancel_order
            action_id: state.action_counter,
            market: ctx.accounts.market.key(),
            owner: order_owner,
            user: acc_manager.key(),
            market_token: Pubkey::default(),
            pricing_token: Pubkey::default(),
            manager: true,
            order_side: side as u8,
            order_id: order_id,
            order_price: order_price,
            order_quantity: order_qty,
            token_withdrawn: tokens_out,
        });

        Ok(())
    }
    
    pub fn extend_log<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ExtendLog<'info>>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let market_state = &ctx.accounts.state;
        let acc_user = &ctx.accounts.user.to_account_info();
        let acc_settle = &ctx.accounts.settle.to_account_info();

        // Verify 
        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;

        let s2 = verify_matching_accounts(&market_state.settle_b, &acc_settle.key, Some(String::from("Settlement log 2")));
        if s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlement log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into());
        }

        // Append a settlement log account
        let state_upd = &mut ctx.accounts.state;
        if !state_upd.log_rollover {
            // Another market participant already appended a new log account (please retry transaction)
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into());
        }
        let av = ctx.remaining_accounts;
        let new_settlement_log = av.get(0).unwrap();
        let market_pk: Pubkey = market.key();
        log_rollover(state_upd, market_pk, acc_settle, new_settlement_log)?;
        log_reimburse(market, state_upd, acc_user)?;
        let mut market_lamports = state_upd.to_account_info().lamports();
        market_lamports = market_lamports.checked_sub(market.log_reimburse).ok_or(error!(ErrorCode::Overflow))?;
        **state_upd.to_account_info().lamports.borrow_mut() = market_lamports;

        Ok(())
    }

    // Withdraw tokens from the settlement vault (manager)
    pub fn manager_withdraw<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ManagerWithdraw<'info>>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_manager = &ctx.accounts.manager.to_account_info();
        let acc_owner = &ctx.accounts.owner.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_settle = &ctx.accounts.settle.to_account_info();
        let acc_settle_prev = &ctx.accounts.settle_prev.to_account_info();
        let acc_settle_next = &ctx.accounts.settle_next.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        // Verify 
        if !market.manager_withdraw {
            msg!("Manager withdrawals disabled");
            return Err(ErrorCode::AccessDenied.into());
        }
        if market.manager != *acc_manager.key {
            msg!("Not manager");
            return Err(ErrorCode::AccessDenied.into());
        }
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;

        state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;

        let mut market_tokens: u64 = 0;
        let mut pricing_tokens: u64 = 0;
        let owner_id: u128 = CritMap::bytes_hash(acc_owner.key.as_ref());
        let log_data: &mut[u8] = &mut acc_settle.try_borrow_mut_data()?;
        let (header, page_table) = mut_array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
        let settle_header: &mut [AccountsHeader] = cast_slice_mut(header);
        verify_matching_accounts(&settle_header[0].market, &market.key(), Some(String::from("Invalid market")))?;
        let close_log: bool = settle_header[0].items == 1 && settle_header[0].prev != Pubkey::default() && settle_header[0].next != Pubkey::default();
        let sl = SlabPageAlloc::new(page_table);
        let has_item = map_get(sl, DT::Account, owner_id);
        if has_item.is_some() {
            let log_node = has_item.unwrap();
            let log_entry = sl.index::<AccountEntry>(SettleDT::Account as u16, log_node.slot() as usize);
            let seeds = &[ctx.accounts.market.to_account_info().key.as_ref(), &[market.agent_nonce]];
            let signer = &[&seeds[..]];
            let mut result = WithdrawResult { mkt_tokens: 0, prc_tokens: 0 };
            if log_entry.mkt_token_balance() > 0 {
                market_tokens = log_entry.mkt_token_balance();
                result.set_mkt_tokens(market_tokens);
                let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, log_entry.mkt_token_balance(),
                    &ctx.accounts.mkt_vault.to_account_info(),          // From
                    &ctx.accounts.user_mkt_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                state.mkt_log_balance = state.mkt_log_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.mkt_vault_balance = state.mkt_vault_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            if log_entry.prc_token_balance() > 0 {
                pricing_tokens = log_entry.prc_token_balance();
                result.set_prc_tokens(pricing_tokens);
                let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, log_entry.prc_token_balance(),
                    &ctx.accounts.prc_vault.to_account_info(),          // From
                    &ctx.accounts.user_prc_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                state.prc_log_balance = state.prc_log_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.prc_vault_balance = state.prc_vault_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            // Remove log entry
            settle_header[0].items = settle_header[0].items.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
            map_remove(sl, DT::Account, log_node.key())?;
            AccountEntry::free_index(sl, DT::Account, log_node.slot())?;
            // Write result
            if *acc_result.key != ctx.accounts.manager.key() {
                store_struct::<WithdrawResult>(&result, acc_result)?;
            }

            // Close log if necessary
            if close_log {
                let log_lamports = log_close(state, acc_settle, acc_settle_prev, acc_settle_next)?;
                let mut market_lamports = state.to_account_info().lamports();
                market_lamports = market_lamports.checked_add(log_lamports).ok_or(error!(ErrorCode::Overflow))?;
                **state.to_account_info().lamports.borrow_mut() = market_lamports;
                state.log_deposit_balance = state.log_deposit_balance.checked_add(log_lamports).ok_or(error!(ErrorCode::Overflow))?;
            }
        } else {
            msg!("Account not found");
            return Err(ErrorCode::AccountNotFound.into());
        }

        msg!("atellix-log");
        emit!(WithdrawEvent {
            event_type: 246174444212986798995680456134066592430, // solana/program/aqua-dex/manager_withdraw
            action_id: state.action_counter,
            market: ctx.accounts.market.key(),
            owner: ctx.accounts.owner.key(),
            user: ctx.accounts.manager.key(),
            market_account: ctx.accounts.user_mkt_token.key(),
            pricing_account: ctx.accounts.user_prc_token.key(),
            manager: true,
            market_tokens: market_tokens,
            pricing_tokens: pricing_tokens,
        });

        Ok(())
    }

    pub fn log_status<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, LogStatus<'info>>) -> anchor_lang::Result<LogStatusResult> {
        let acc_settle = &ctx.accounts.settle.to_account_info();
        let log_data: &[u8] = &acc_settle.try_borrow_data()?;
        let (header, _page_table) = array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
        let settle_header: &[AccountsHeader] = cast_slice(header);
        Ok(LogStatusResult {
            prev: settle_header[0].prev,
            next: settle_header[0].next,
            items: settle_header[0].items,
        })
    }

    // Deposit or withdraw lamports for settlement log accounts and reimbursements
    pub fn manager_transfer_sol<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ManagerTransferSol<'info>>,
        inp_withdraw: bool,
        inp_all: bool,
        inp_amount: u64,
    ) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let acc_manager = &ctx.accounts.manager.to_account_info();

        if market.manager != *acc_manager.key {
            msg!("Not manager");
            return Err(ErrorCode::AccessDenied.into());
        }
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;
        let mut market_lamports = state.to_account_info().lamports();
        let mut manager_lamports = acc_manager.lamports();

        if inp_withdraw {
            let withdraw_amount: u64;
            if inp_all {
                withdraw_amount = state.log_deposit_balance;
            } else {
                withdraw_amount = inp_amount;
            }
            state.log_deposit_balance = state.log_deposit_balance.checked_sub(withdraw_amount).ok_or(error!(ErrorCode::Overflow))?;
            market_lamports = market_lamports.checked_sub(withdraw_amount).ok_or(error!(ErrorCode::Overflow))?;
            manager_lamports = manager_lamports.checked_add(withdraw_amount).ok_or(error!(ErrorCode::Overflow))?;
        } else { // Deposit lamports
            manager_lamports = manager_lamports.checked_sub(inp_amount).ok_or(error!(ErrorCode::Overflow))?;
            market_lamports = market_lamports.checked_add(inp_amount).ok_or(error!(ErrorCode::Overflow))?;
            state.log_deposit_balance = state.log_deposit_balance.checked_add(inp_amount).ok_or(error!(ErrorCode::Overflow))?;
        }

        **state.to_account_info().lamports.borrow_mut() = market_lamports;
        **acc_manager.lamports.borrow_mut() = manager_lamports;
 
        Ok(())
    }

    pub fn manager_withdraw_fees<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ManagerWithdrawFees<'info>>) -> anchor_lang::Result<u64> {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_manager = &ctx.accounts.manager.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();

        if market.manager != *acc_manager.key {
            msg!("Not manager");
            return Err(ErrorCode::AccessDenied.into());
        }
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;

        let fee_tokens = state.prc_fees_balance;
        if fee_tokens > 0 {
            state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;
            state.prc_fees_balance = 0;

            let seeds = &[market.to_account_info().key.as_ref(), &[market.agent_nonce]];
            let signer = &[&seeds[..]];
            let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
            perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, fee_tokens, 
                &ctx.accounts.prc_vault.to_account_info(),          // From
                &ctx.accounts.manager_prc_token.to_account_info(),  // To
                &ctx.accounts.agent.to_account_info(),              // Auth
                &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
            )?;

            msg!("atellix-log");
            emit!(WithdrawEvent {
                event_type: 68727559793861179499689993618056023286, // solana/program/aqua-dex/manager_withdraw/fees
                action_id: state.action_counter,
                market: ctx.accounts.market.key(),
                owner: Pubkey::default(),
                user: ctx.accounts.manager.key(),
                market_account: Pubkey::default(),
                pricing_account: ctx.accounts.manager_prc_token.key(),
                manager: true,
                market_tokens: 0,
                pricing_tokens: fee_tokens,
            });
        }
        Ok(fee_tokens)
    }

    pub fn manager_update_market<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ManagerUpdateMarket<'info>>,
        inp_active: bool,
        inp_expire_enable: bool,
        inp_expire_min: i64,
        inp_min_quantity: u64,
        inp_taker_fee: u32,
        inp_log_fee: u64,
        inp_log_rebate: u64,
        inp_log_reimburse: u64,
    ) -> anchor_lang::Result<()> {
        let market = &mut ctx.accounts.market;
        let acc_manager = &ctx.accounts.manager.to_account_info();

        if market.manager != *acc_manager.key {
            msg!("Not manager");
            return Err(ErrorCode::AccessDenied.into());
        }
        
        market.active = inp_active;
        market.expire_enable = inp_expire_enable;
        market.expire_min = inp_expire_min;
        market.min_quantity = inp_min_quantity;
        market.taker_fee = inp_taker_fee;
        market.log_fee = inp_log_fee;
        market.log_rebate = inp_log_rebate;
        market.log_reimburse = inp_log_reimburse;

        Ok(())
    }

    // Create user vaults (manager only)
    pub fn create_vault<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, CreateVault<'info>>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let vault = &mut ctx.accounts.vault;
        let acc_manager = &ctx.accounts.manager.to_account_info();

        if market.manager != *acc_manager.key {
            msg!("Not manager");
            return Err(ErrorCode::AccessDenied.into());
        }

        if !vault.initialized { // Only initialize once
            vault.initialized = true;
            vault.market = ctx.accounts.market.key();
            vault.owner = ctx.accounts.owner.key();
            vault.mkt_tokens = 0;
            vault.prc_tokens = 0;
        }
        Ok(())
    }
 
    // Move tokens from the settlement log to a user's individual vault (vault manager only)
    // This is optional market "housekeeping". If a market manager moves balances from the settlement logs to user vaults before the
    // 1st settlement log file fills up then there will never be a need to rollover settlement logs and possibly require repeating trade transactions.
    // If markets opt out of this housekeeping then the settlement log will grow if more and more users leave settled trades in the logs.
    // Traders will need to keep their "Market Data" account up-to-date as settlement log rollovers occur unless managers do housekeeping.
    // Resending trades is automated in the API frontend but requires possibly resending transactions. (Based on "RetrySettlementAccount" error.)
    // Keeping open slots in the 1st settlement log enables all trades to be processed immediately without any need of a possible data refresh.
    pub fn vault_deposit<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, VaultDeposit<'info>>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let vault = &mut ctx.accounts.vault;
        let acc_manager = &ctx.accounts.manager.to_account_info();
        let acc_owner = &ctx.accounts.owner.to_account_info();
        let acc_settle = &ctx.accounts.settle.to_account_info();
        let acc_settle_prev = &ctx.accounts.settle_prev.to_account_info();
        let acc_settle_next = &ctx.accounts.settle_next.to_account_info();

        // Verify
        if market.manager != *acc_manager.key {
            msg!("Not manager");
            return Err(ErrorCode::AccessDenied.into());
        }
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;

        let mut market_tokens: u64 = 0;
        let mut pricing_tokens: u64 = 0;
        let owner_id: u128 = CritMap::bytes_hash(acc_owner.key.as_ref());
        let log_data: &mut[u8] = &mut acc_settle.try_borrow_mut_data()?;
        let (header, page_table) = mut_array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
        let settle_header: &mut [AccountsHeader] = cast_slice_mut(header);
        let close_log: bool = settle_header[0].items == 1 && settle_header[0].prev != Pubkey::default() && settle_header[0].next != Pubkey::default();
        verify_matching_accounts(&settle_header[0].market, &market.key(), Some(String::from("Invalid market")))?;
        let sl = SlabPageAlloc::new(page_table);
        let has_item = map_get(sl, DT::Account, owner_id);
        if has_item.is_some() {
            let log_node = has_item.unwrap();
            let log_entry = sl.index::<AccountEntry>(SettleDT::Account as u16, log_node.slot() as usize);
            if log_entry.mkt_token_balance() > 0 {
                market_tokens = log_entry.mkt_token_balance();
                state.mkt_log_balance = state.mkt_log_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.mkt_user_vault_balance = state.mkt_user_vault_balance.checked_add(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
                vault.mkt_tokens = vault.mkt_tokens.checked_add(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            if log_entry.prc_token_balance() > 0 {
                pricing_tokens = log_entry.prc_token_balance();
                state.prc_log_balance = state.prc_log_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.prc_user_vault_balance = state.prc_user_vault_balance.checked_add(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
                vault.prc_tokens = vault.prc_tokens.checked_add(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            // Remove log entry
            settle_header[0].items = settle_header[0].items.checked_sub(1).ok_or(error!(ErrorCode::Overflow))?;
            map_remove(sl, DT::Account, log_node.key())?;
            AccountEntry::free_index(sl, DT::Account, log_node.slot())?;

            // Close log if necessary
            if close_log {
                let log_lamports = log_close(state, acc_settle, acc_settle_prev, acc_settle_next)?;
                let mut market_lamports = state.to_account_info().lamports();
                market_lamports = market_lamports.checked_add(log_lamports).ok_or(error!(ErrorCode::Overflow))?;
                **state.to_account_info().lamports.borrow_mut() = market_lamports;
                state.log_deposit_balance = state.log_deposit_balance.checked_add(log_lamports).ok_or(error!(ErrorCode::Overflow))?;
            }
        } else {
            msg!("Account not found");
            return Err(ErrorCode::AccountNotFound.into());
        }

        if market_tokens > 0 || pricing_tokens > 0 {
            state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;

            msg!("atellix-log");
            emit!(VaultDepositEvent {
                event_type: 116949236330450057903776475751429156227, // solana/program/aqua-dex/user_vault/deposit
                action_id: state.action_counter,
                market: market.key(),
                owner: *acc_owner.key,
                vault: vault.key(),
                market_tokens: market_tokens,
                market_balance: vault.mkt_tokens,
                pricing_tokens: pricing_tokens,
                pricing_balance: vault.prc_tokens,
            });
        }

        Ok(())
    }

    // Users can withdraw tokens from their own vaults
    pub fn vault_withdraw<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, VaultWithdraw<'info>>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let vault = &mut ctx.accounts.vault;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_owner = &ctx.accounts.owner.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();

        // Verify 
        if vault.owner != *acc_owner.key {
            msg!("Not owner");
            return Err(ErrorCode::AccessDenied.into());
        }
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;

        if vault.mkt_tokens > 0 || vault.prc_tokens > 0 {
            state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;

            let mut market_tokens: u64 = 0;
            let mut pricing_tokens: u64 = 0;
            let seeds = &[ctx.accounts.market.to_account_info().key.as_ref(), &[market.agent_nonce]];
            let signer = &[&seeds[..]];
            if vault.mkt_tokens > 0 {
                market_tokens = vault.mkt_tokens;
                let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, market_tokens,
                    &ctx.accounts.mkt_vault.to_account_info(),          // From
                    &ctx.accounts.user_mkt_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                vault.mkt_tokens = 0;
                state.mkt_vault_balance = state.mkt_vault_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.mkt_user_vault_balance = state.mkt_user_vault_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            if vault.prc_tokens > 0 {
                pricing_tokens = vault.prc_tokens;
                let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, pricing_tokens,
                    &ctx.accounts.prc_vault.to_account_info(),          // From
                    &ctx.accounts.user_prc_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                vault.prc_tokens = 0;
                state.prc_vault_balance = state.prc_vault_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.prc_user_vault_balance = state.prc_user_vault_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }

            // Close the vault and transfer lamports to the market
            let vault_lamports = vault.to_account_info().lamports();
            **vault.to_account_info().lamports.borrow_mut() = 0;
            let mut market_lamports = state.to_account_info().lamports();
            market_lamports = market_lamports.checked_add(vault_lamports).ok_or(error!(ErrorCode::Overflow))?;
            **state.to_account_info().lamports.borrow_mut() = market_lamports;
            state.log_deposit_balance = state.log_deposit_balance.checked_add(vault_lamports).ok_or(error!(ErrorCode::Overflow))?;

            msg!("atellix-log");
            emit!(VaultWithdrawEvent {
                event_type: 222531087088795477156040686028020078326, // solana/program/aqua-dex/user_vault/withdraw
                action_id: state.action_counter,
                market: market.key(),
                owner: *acc_owner.key,
                user: *acc_owner.key,
                vault: vault.key(),
                market_account: ctx.accounts.user_mkt_token.key(),
                pricing_account: ctx.accounts.user_prc_token.key(),
                manager: false,
                market_tokens: market_tokens,
                pricing_tokens: pricing_tokens,
            });
        }

        Ok(())
    }

    // Manager withdrawal from user vaults
    pub fn manager_vault_withdraw<'a, 'b, 'c, 'info>(ctx: Context<'a, 'b, 'c, 'info, ManagerVaultWithdraw<'info>>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let vault = &mut ctx.accounts.vault;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_owner = &ctx.accounts.owner.to_account_info();
        let acc_manager = &ctx.accounts.manager.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();

        // Verify 
        if !market.manager_withdraw {
            msg!("Manager withdrawals disabled");
            return Err(ErrorCode::AccessDenied.into());
        }
        if market.manager != *acc_manager.key {
            msg!("Not owner");
            return Err(ErrorCode::AccessDenied.into());
        }
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;

        if vault.mkt_tokens > 0 || vault.prc_tokens > 0 {
            state.action_counter = state.action_counter.checked_add(1).ok_or(error!(ErrorCode::Overflow))?;

            let mut market_tokens: u64 = 0;
            let mut pricing_tokens: u64 = 0;
            let seeds = &[ctx.accounts.market.to_account_info().key.as_ref(), &[market.agent_nonce]];
            let signer = &[&seeds[..]];
            if vault.mkt_tokens > 0 {
                market_tokens = vault.mkt_tokens;
                let mint_type = MintType::try_from(market.mkt_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, market_tokens,
                    &ctx.accounts.mkt_vault.to_account_info(),          // From
                    &ctx.accounts.user_mkt_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                vault.mkt_tokens = 0;
                state.mkt_vault_balance = state.mkt_vault_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.mkt_user_vault_balance = state.mkt_user_vault_balance.checked_sub(market_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }
            if vault.prc_tokens > 0 {
                pricing_tokens = vault.prc_tokens;
                let mint_type = MintType::try_from(market.prc_mint_type).map_err(|_| ErrorCode::InvalidParameters)?;
                perform_signed_transfer(ctx.remaining_accounts, signer, mint_type, 0, pricing_tokens,
                    &ctx.accounts.prc_vault.to_account_info(),          // From
                    &ctx.accounts.user_prc_token.to_account_info(),     // To
                    &ctx.accounts.agent.to_account_info(),              // Auth
                    &ctx.accounts.spl_token_prog.to_account_info(),     // SPL Token Program
                )?;
                vault.prc_tokens = 0;
                state.prc_vault_balance = state.prc_vault_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
                state.prc_user_vault_balance = state.prc_user_vault_balance.checked_sub(pricing_tokens).ok_or(error!(ErrorCode::Overflow))?;
            }

            // Close the vault and transfer lamports to the market
            let vault_lamports = vault.to_account_info().lamports();
            **vault.to_account_info().lamports.borrow_mut() = 0;
            let mut market_lamports = state.to_account_info().lamports();
            market_lamports = market_lamports.checked_add(vault_lamports).ok_or(error!(ErrorCode::Overflow))?;
            **state.to_account_info().lamports.borrow_mut() = market_lamports;
            state.log_deposit_balance = state.log_deposit_balance.checked_add(vault_lamports).ok_or(error!(ErrorCode::Overflow))?;

            msg!("atellix-log");
            emit!(VaultWithdrawEvent {
                event_type: 155648231829618734246883800498177854177, // solana/program/aqua-dex/user_vault/manager_withdraw
                action_id: state.action_counter,
                market: market.key(),
                owner: *acc_owner.key,
                user: *acc_manager.key,
                vault: vault.key(),
                market_account: ctx.accounts.user_mkt_token.key(),
                pricing_account: ctx.accounts.user_prc_token.key(),
                manager: true,
                market_tokens: market_tokens,
                pricing_tokens: pricing_tokens,
            });
        }

        Ok(())
    }

    pub fn close_vault(ctx: Context<CloseVault>) -> anchor_lang::Result<()> {
        let market = &ctx.accounts.market;
        let vault = &mut ctx.accounts.vault;
        let acc_manager = &ctx.accounts.manager.to_account_info();

        if market.manager != *acc_manager.key {
            msg!("Not vault manager");
            return Err(ErrorCode::AccessDenied.into());
        }
        if vault.mkt_tokens > 0 || vault.prc_tokens > 0 {
            msg!("Vault not empty");
            return Err(ErrorCode::VaultNotEmpty.into());
        }

        Ok(())
    }

    pub fn close_trade_result(_ctx: Context<CloseTradeResult>) -> anchor_lang::Result<()> {
        Ok(())
    }

    pub fn close_withdraw_result(_ctx: Context<CloseWithdrawResult>) -> anchor_lang::Result<()> {
        Ok(())
    }
}

#[derive(Accounts)]
pub struct UpdateMetadata<'info> {
    #[account(constraint = program.programdata_address().unwrap() == Some(program_data.key()))]
    pub program: Program<'info, AquaDex>,
    #[account(constraint = program_data.upgrade_authority_address == Some(program_admin.key()))]
    pub program_data: Account<'info, ProgramData>,
    #[account(mut)]
    pub program_admin: Signer<'info>,
    #[account(init_if_needed, seeds = [program_id.as_ref(), b"metadata"], bump, payer = program_admin, space = 584)]
    pub program_info: Account<'info, ProgramMetadata>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(inp_agent_nonce: u8)]
pub struct CreateMarket<'info> {
    /// CHECK: ok
    #[account(zero)]
    pub market: AccountInfo<'info>, 
    /// CHECK: ok
    #[account(zero)]
    pub state: AccountInfo<'info>,
    /// CHECK: ok
    #[account(seeds = [market.key().as_ref()], bump = inp_agent_nonce)]
    pub agent: AccountInfo<'info>,
    #[account(mut)]
    pub manager: Signer<'info>,
    /// CHECK: ok
    pub mkt_mint: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    /// CHECK: ok
    pub prc_mint: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub trade_log: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub orders: AccountInfo<'info>,
    /// CHECK: ok
    #[account(zero)]
    pub settle_a: AccountInfo<'info>,
    /// CHECK: ok
    #[account(zero)]
    pub settle_b: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = associated_token::ID)]
    /// CHECK: ok
    pub asc_token_prog: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = system_program::ID)]
    pub system_program: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = sysvar::rent::ID)]
    pub system_rent: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct OrderContext<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    pub agent: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub user: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>, // Deposit market tokens for "Ask" orders
    /// CHECK: ok
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>, // Withdraw pricing tokens if the order is filled or partially filled
    /// CHECK: ok
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub orders: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub trade_log: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_a: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_b: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub result: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CancelOrder<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    pub agent: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub owner: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub orders: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub result: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ManagerCancelOrder<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub orders: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub result: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_a: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_b: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ExpireOrder<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub user: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub orders: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_a: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_b: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    pub agent: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub owner: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_prev: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_next: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub result: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ManagerWithdraw<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    pub agent: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
    /// CHECK: ok
    pub owner: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_prev: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_next: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub result: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct LogStatus<'info> {
    /// CHECK: ok
    pub settle: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CreateVault<'info> {
    pub market: Account<'info, Market>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
    /// CHECK: ok
    pub owner: AccountInfo<'info>,
    #[account(init_if_needed, seeds = [market.key().as_ref(), owner.key().as_ref()], bump, payer = manager, space = 89)]
    pub vault: Account<'info, UserVault>,
    /// CHECK: ok
    #[account(address = system_program::ID)]
    pub system_program: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct VaultDeposit<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    #[account(signer)]
    pub manager: AccountInfo<'info>,
    /// CHECK: ok
    pub owner: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_prev: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle_next: AccountInfo<'info>,
    #[account(mut, seeds = [market.key().as_ref(), owner.key().as_ref()], bump)]
    pub vault: Account<'info, UserVault>,
}

#[derive(Accounts)]
pub struct VaultWithdraw<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    pub agent: AccountInfo<'info>,
    /// CHECK: ok
    #[account(signer)]
    pub owner: AccountInfo<'info>,
    #[account(mut, seeds = [market.key().as_ref(), owner.key().as_ref()], bump)]
    pub vault: Account<'info, UserVault>,
    /// CHECK: ok
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub result: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ManagerTransferSol<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ManagerWithdrawFees<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    pub agent: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub manager_prc_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ManagerUpdateMarket<'info> {
    #[account(mut)]
    pub market: Account<'info, Market>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ManagerVaultWithdraw<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    pub agent: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
    /// CHECK: ok
    pub owner: AccountInfo<'info>,
    #[account(mut, seeds = [market.key().as_ref(), owner.key().as_ref()], bump)]
    pub vault: Account<'info, UserVault>,
    /// CHECK: ok
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub result: AccountInfo<'info>,
    /// CHECK: ok
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CloseVault<'info> {
    pub market: Account<'info, Market>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
    /// CHECK: ok
    pub owner: AccountInfo<'info>,
    #[account(mut, seeds = [market.key().as_ref(), owner.key().as_ref()], bump, close = fee_receiver)]
    pub vault: Account<'info, UserVault>,
    /// CHECK: ok
    #[account(mut)]
    pub fee_receiver: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CloseTradeResult<'info> {
    /// CHECK: ok
    #[account(mut)]
    pub fee_receiver: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer, close = fee_receiver)]
    pub result: Account<'info, TradeResult>,
}

#[derive(Accounts)]
pub struct CloseWithdrawResult<'info> {
    /// CHECK: ok
    #[account(mut)]
    pub fee_receiver: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut, signer, close = fee_receiver)]
    pub result: Account<'info, WithdrawResult>,
}

#[derive(Accounts)]
pub struct ExtendLog<'info> {
    pub market: Account<'info, Market>,
    #[account(mut)]
    pub state: Account<'info, MarketState>,
    /// CHECK: ok
    #[account(mut, signer)]
    pub user: AccountInfo<'info>,
    /// CHECK: ok
    #[account(mut)]
    pub settle: AccountInfo<'info>,
}

#[account]
pub struct ProgramMetadata {
    pub semvar_major: u32,
    pub semvar_minor: u32,
    pub semvar_patch: u32,
    pub program: Pubkey,
    pub program_name: String,   // Max len 60
    pub developer_name: String, // Max len 60
    pub developer_url: String,  // Max len 124
    pub source_url: String,     // Max len 124
    pub verify_url: String,     // Max len 124
}
// 8 + (4 * 3) + 32 + (4 * 5) + (64 * 2) + (128 * 3)
// Data length (with discrim): 584 bytes

#[account]
pub struct Market {
    pub active: bool,                   // Active flag
    pub manager_withdraw: bool,         // Allow "manager_withdraw" and "manager_vault_withdraw" to let the market manager withdraw user tokens (FALSE for trustless mode)
    pub manager_cancel: bool,           // Allow "manager_cancel_order" to let the market manager cancel orders and move user tokens to the settlement log (FALSE for trustless mode)
    pub expire_enable: bool,            // Enable order expiration
    pub expire_min: i64,                // Minimum time an order must be posted before expiration
    pub min_quantity: u64,              // Minimum quantity to trade (0 for no minimum)
    pub log_fee: u64,                   // Fee for settlement log space for posted orders (lamports)
    pub log_rebate: u64,                // Rebate for withdrawal (lamports)
    pub log_reimburse: u64,             // Reimbursement for adding a new settlement log (lamports)
    pub taker_fee: u32,                 // Taker commission fee
    pub state: Pubkey,                  // Market statistics (frequently updated market details)
    pub trade_log: Pubkey,              // Trade log
    pub agent: Pubkey,                  // Program derived address for signing transfers
    pub agent_nonce: u8,                // Agent nonce
    pub manager: Pubkey,                // Market manager
    pub mkt_mint: Pubkey,               // Token mint for market tokens (Token A)
    pub mkt_vault: Pubkey,              // Vault for Token A (an associated token account controlled by this program)
    pub mkt_nonce: u8,                  // Vault nonce for Token A
    pub mkt_decimals: u8,               // Token A decimals
    pub mkt_mint_type: u8,              // Token A mint type
    pub prc_mint: Pubkey,               // Token mint for pricing tokens (Token B)
    pub prc_vault: Pubkey,              // Vault for Token B
    pub prc_nonce: u8,                  // Vault nonce for Token B
    pub prc_decimals: u8,               // Token B decimals
    pub prc_mint_type: u8,              // Token B mint type
    pub orders: Pubkey,                 // Orderbook Bid/Ask entries
    pub settle_0: Pubkey,               // The start of the settlement log
}

#[account]
pub struct MarketState {
    pub settle_a: Pubkey,               // Settlement log 1 (the active log)
    pub settle_b: Pubkey,               // Settlement log 2 (the next log)
    pub log_rollover: bool,             // Request for a new settlement log account for rollover
    pub log_deposit_balance: u64,       // Lamports deposited for allocate new settlement log space
    pub action_counter: u64,            // Action ids
    pub trade_counter: u64,             // Trade ids
    pub order_counter: u64,             // Order index for Critmap ids (lower 64 bits)
    pub active_bid: u64,                // Active bid orders in the orderbook
    pub active_ask: u64,                // Active ask orders in the orderbook
    pub mkt_vault_balance: u64,         // Token A vault total balance (including tokens available to withdraw)
    pub mkt_order_balance: u64,         // Token A order balance (tokens in vault available to trade)
    pub mkt_user_vault_balance: u64,    // Token A order balance (tokens in vault available to trade)
    pub mkt_log_balance: u64,           // Token A balance in the settlement log
    pub prc_vault_balance: u64,         // Token B vault total balance
    pub prc_order_balance: u64,         // Token B order balance
    pub prc_user_vault_balance: u64,    // Token B user vault balance
    pub prc_log_balance: u64,           // Token B balance in the settlement log
    pub prc_fees_balance: u64,          // Token B commission fees balance
    pub last_ts: i64,                   // Timestamp of last event (market created or order filled)
    pub last_price: u64,                // Last price (Do not use as an oracle value, prices should be averaged over some period of time for that purpose.)
}

#[account]
pub struct UserVault {
    pub initialized: bool,              // Initialized
    pub market: Pubkey,                 // Market
    pub owner: Pubkey,                  // Owner
    pub mkt_tokens: u64,                // Market tokens in the user's vault
    pub prc_tokens: u64,                // Pricing tokens in the user's vault
}
// Size: 8 + 1 + 32 + 32 + 8 + 8 = 89

#[account]
pub struct TradeResult {
    pub tokens_received: u64,           // Received tokens
    pub tokens_sent: u64,               // Tokens deposited with the exchange (filled token cost + tokens posted)
    pub tokens_fee: u64,                // Taker commission fee
    pub posted_quantity: u64,           // Posted token quantity
    pub order_id: u128,                 // Order ID
}

impl TradeResult {
    pub fn set_tokens_received(&mut self, new_amount: u64) {
        self.tokens_received = new_amount;
    }

    pub fn set_posted_quantity(&mut self, new_amount: u64) {
        self.posted_quantity = new_amount;
    }

    pub fn set_tokens_sent(&mut self, new_amount: u64) {
        self.tokens_sent = new_amount;
    }

    pub fn set_order_id(&mut self, new_amount: u128) {
        self.order_id = new_amount;
    }
}

#[account]
pub struct WithdrawResult {
    pub mkt_tokens: u64,                // Market tokens
    pub prc_tokens: u64,                // Pricing tokens
}

impl WithdrawResult {
    pub fn set_mkt_tokens(&mut self, new_amount: u64) {
        self.mkt_tokens = new_amount;
    }

    pub fn set_prc_tokens(&mut self, new_amount: u64) {
        self.prc_tokens = new_amount;
    }
}

#[account]
pub struct LogStatusResult {
    pub prev: Pubkey,
    pub next: Pubkey,
    pub items: u32,
}

#[event]
pub struct MatchEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub trade_id: u64,
    pub market: Pubkey,
    pub maker_order_id: u128,
    pub maker_filled: bool,
    pub maker: Pubkey,
    pub taker: Pubkey,
    pub taker_side: u8,
    pub amount: u64,
    pub price: u64,
    pub ts: i64,
}

#[event]
pub struct OrderEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub market: Pubkey,
    pub user: Pubkey,
    pub market_token: Pubkey,
    pub pricing_token: Pubkey,
    pub order_side: u8,
    pub order_id: u128,
    pub filled: bool,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub tokens_fee: u64,
    pub posted: bool,
    pub posted_quantity: u64,
    pub order_price: u64,
    pub order_quantity: u64,
    pub expires: i64,
}

#[event]
pub struct CancelEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub user: Pubkey,
    pub market_token: Pubkey,
    pub pricing_token: Pubkey,
    pub manager: bool,
    pub order_side: u8,
    pub order_id: u128,
    pub order_price: u64,
    pub order_quantity: u64,
    pub token_withdrawn: u64,
}

#[event]
pub struct ExpireEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub order_side: u8,
    pub order_id: u128,
    pub price: u64,
    pub quantity: u64,
    pub tokens: u64,
}

#[event]
pub struct WithdrawEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub user: Pubkey,
    pub market_account: Pubkey,
    pub pricing_account: Pubkey,
    pub manager: bool,
    pub market_tokens: u64,
    pub pricing_tokens: u64,
}

#[event]
pub struct SettleEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub settlement_log: Pubkey,
    pub market_tokens: u64,
    pub pricing_tokens: u64,
}

#[event]
pub struct VaultDepositEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub vault: Pubkey,
    pub market_tokens: u64,
    pub market_balance: u64,
    pub pricing_tokens: u64,
    pub pricing_balance: u64,
}

#[event]
pub struct VaultWithdrawEvent {
    pub event_type: u128,
    pub action_id: u64,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub user: Pubkey,
    pub vault: Pubkey,
    pub market_account: Pubkey,
    pub pricing_account: Pubkey,
    pub manager: bool,
    pub market_tokens: u64,
    pub pricing_tokens: u64,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Access denied")]
    AccessDenied,
    #[msg("Token account frozen")]
    TokenAccountFrozen,
    #[msg("Insufficient tokens")]
    InsufficientTokens,
    #[msg("Market closed")]
    MarketClosed,
    #[msg("Account not found")]
    AccountNotFound,
    #[msg("Record not found")]
    RecordNotFound,
    #[msg("Order not found")]
    OrderNotFound,
    #[msg("Invalid parameters")]
    InvalidParameters,
    #[msg("Invalid account")]
    InvalidAccount,
    #[msg("Invalid derived account")]
    InvalidDerivedAccount,
    #[msg("Vault not empty")]
    VaultNotEmpty,
    #[msg("Order not filled")]
    OrderNotFilled,
    #[msg("Internal error")]
    InternalError,
    #[msg("External error")]
    ExternalError,
    #[msg("Settlement log full")]
    SettlementLogFull,
    #[msg("Settlement log not empty")]
    SettlementLogNotEmpty,
    #[msg("Orderbook full")]
    OrderbookFull,
    #[msg("Settlement log account does not match market, please update market data and retry")]
    RetrySettlementAccount,
    #[msg("Quantity below minimum")]
    QuantityBelowMinimum,
    #[msg("Overflow")]
    Overflow,
}

