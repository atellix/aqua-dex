use std::{ io::Cursor, string::String, str::FromStr, result::Result as FnResult, mem::size_of, convert::TryFrom };
use bytemuck::{ Pod, Zeroable, cast_slice_mut };
use byte_slice_cast::*;
use num_enum::{ TryFromPrimitive, IntoPrimitive };
use arrayref::{ array_refs, mut_array_refs };
use anchor_lang::prelude::*;
use anchor_spl::token::{ self, Transfer };
use solana_program::{
    sysvar, system_instruction, system_program,
    program::{ invoke, invoke_signed },
    account_info::AccountInfo,
    instruction::{ AccountMeta, Instruction }
};

extern crate slab_alloc;
use slab_alloc::{ SlabPageAlloc, CritMapHeader, CritMap, AnyNode, LeafNode, SlabVec, SlabTreeError };

pub const MAX_ORDERS: u32 = 16;         // Max orders on each side of the orderbook
pub const MAX_ACCOUNTS: u32 = 16;       // Max number of accounts per settlement data file
pub const MAX_EVICTIONS: u32 = 10;      // Max number of orders to evict before aborting
pub const SPL_TOKEN_PK: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ASC_TOKEN_PK: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

#[repr(u8)]
#[derive(PartialEq, Debug, Eq, Copy, Clone)]
pub enum Side {
    Bid,
    Ask,
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
        let free_index = free_top.checked_sub(1).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        let index_act = pt.index::<Order>(index_datatype(data_type), free_index as usize);
        let index_ptr = u32::try_from(index_act.amount()).expect("Invalid index");
        pt.header_mut::<SlabVec>(index_datatype(data_type)).set_free_top(index_ptr);
        Ok(free_index)
    }

    pub fn free_index(pt: &mut SlabPageAlloc, data_type: DT, idx: u32) -> ProgramResult {
        let free_top = pt.header::<SlabVec>(index_datatype(data_type)).free_top();
        pt.index_mut::<Order>(index_datatype(data_type), idx as usize).set_amount(free_top as u64);
        let new_top = idx.checked_add(1).ok_or(ProgramError::from(ErrorCode::Overflow))?;
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
}
unsafe impl Zeroable for AccountsHeader {}
unsafe impl Pod for AccountsHeader {}

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct AccountEntry {
    pub mkt_token_balance: u64,
    pub prc_token_balance: u64,
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

    pub fn set_mkt_token_balance(&mut self, bal: u64) {
        self.mkt_token_balance = bal;
    }

    pub fn set_prc_token_balance(&mut self, bal: u64) {
        self.prc_token_balance = bal;
    }

    fn next_index(pt: &mut SlabPageAlloc, data_type: DT) -> FnResult<u32, ProgramError> {
        let svec = pt.header_mut::<SlabVec>(index_datatype(data_type));
        let free_top = svec.free_top();
        if free_top == 0 { // Empty free list
            return Ok(svec.next_index());
        }
        let free_index = free_top.checked_sub(1).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        let index_act = pt.index::<AccountEntry>(index_datatype(data_type), free_index as usize);
        let index_ptr = u32::try_from(index_act.mkt_token_balance()).expect("Invalid index");
        pt.header_mut::<SlabVec>(index_datatype(data_type)).set_free_top(index_ptr);
        Ok(free_index)
    }

    fn free_index(pt: &mut SlabPageAlloc, data_type: DT, idx: u32) -> ProgramResult {
        let free_top = pt.header::<SlabVec>(index_datatype(data_type)).free_top();
        pt.index_mut::<AccountEntry>(index_datatype(data_type), idx as usize).set_mkt_token_balance(free_top as u64);
        let new_top = idx.checked_add(1).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        pt.header_mut::<SlabVec>(index_datatype(data_type)).set_free_top(new_top);
        Ok(())
    }
}

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
fn map_predicate_min<F: Fn(&SlabPageAlloc, &LeafNode) -> bool>(pt: &mut SlabPageAlloc, data_type: DT, predicate: F) -> Option<LeafNode> {
    let cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.predicate_min(predicate);
    match res {
        None => None,
        Some(res) => Some(res.clone()),
    }
}

#[inline]
fn map_predicate_max<F: Fn(&SlabPageAlloc, &LeafNode) -> bool>(pt: &mut SlabPageAlloc, data_type: DT, predicate: F) -> Option<LeafNode> {
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
fn map_remove(pt: &mut SlabPageAlloc, data_type: DT, key: u128) -> FnResult<(), SlabTreeError> {
    let mut cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let res = cm.remove_by_key(key).ok_or(SlabTreeError::NotFound)?;
    Ok(())
}

#[inline]
fn store_struct<T: AccountSerialize>(obj: &T, acc: &AccountInfo) -> FnResult<(), ProgramError> {
    let mut data = acc.try_borrow_mut_data()?;
    let dst: &mut [u8] = &mut data;
    let mut crs = Cursor::new(dst);
    obj.try_serialize(&mut crs)
}

fn verify_matching_accounts(left: &Pubkey, right: &Pubkey, error_msg: Option<String>) -> ProgramResult {
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

fn log_settlement(
    state: &mut MarketState, 
    settle_a: &AccountInfo,
    settle_b: &AccountInfo,
    owner: &Pubkey,
    mkt_token: bool,
    amount: u64,
) -> ProgramResult {
    // TODO: use log B
    let owner_key: u128 = CritMap::bytes_hash(owner.as_ref());
    let mut new_balance: u64 = amount; // Fix
    let log_data: &mut[u8] = &mut settle_a.try_borrow_mut_data()?;
    let (_header, page_table) = mut_array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
    let sl = SlabPageAlloc::new(page_table);
    let has_item = map_get(sl, DT::Account, owner_key);
    if has_item.is_none() {
        new_balance = amount;
        let acct_idx = AccountEntry::next_index(sl, DT::Account)?;
        let new_item = map_insert(sl, DT::Account, &LeafNode::new(owner_key, acct_idx, owner));
        if new_item.is_ok() {
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
            };
            *sl.index_mut::<AccountEntry>(SettleDT::Account.into(), acct_idx as usize) = acct;
        } else {
            // TODO: rollover to next log
            msg!("Settlement log full");
            return Err(ErrorCode::InternalError.into());
        }
    } else {
        let log_item = has_item.unwrap();
        let current_acct = sl.index::<AccountEntry>(SettleDT::Account.into(), 0 as usize);
        let mut mkt_bal: u64 = current_acct.mkt_token_balance;
        let mut prc_bal: u64 = current_acct.prc_token_balance;
        if mkt_token {
            mkt_bal = mkt_bal.checked_add(amount).ok_or(ProgramError::from(ErrorCode::Overflow))?;
            sl.index_mut::<AccountEntry>(SettleDT::Account.into(), 0 as usize).set_mkt_token_balance(mkt_bal);
            new_balance = mkt_bal;
        } else {
            prc_bal = prc_bal.checked_add(amount).ok_or(ProgramError::from(ErrorCode::Overflow))?;
            sl.index_mut::<AccountEntry>(SettleDT::Account.into(), 0 as usize).set_prc_token_balance(prc_bal);
            new_balance = prc_bal;
        }
    }

    if mkt_token {
        msg!("Atellix: Settle Market Token - Amt: {} Bal: {} Key: {}", amount.to_string(), new_balance.to_string(), owner.to_string());
    } else {
        msg!("Atellix: Settle Pricing Token - Amt: {} Bal: {} Key: {}", amount.to_string(), new_balance.to_string(), owner.to_string());
    }

    if mkt_token {
        state.mkt_order_balance = state.mkt_order_balance.checked_sub(amount).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        /*msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
            state.mkt_vault_balance.to_string(),
            state.mkt_order_balance.to_string(),
        );*/
    } else {
        state.prc_order_balance = state.prc_order_balance.checked_sub(amount).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        /*msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
            state.prc_vault_balance.to_string(),
            state.prc_order_balance.to_string(),
        );*/
    }

    Ok(())
}

#[program]
pub mod aqua_dex {
    use super::*;

    pub fn create_market(ctx: Context<CreateMarket>,
        inp_agent_nonce: u8,
        inp_mkt_vault_nonce: u8,
        inp_prc_vault_nonce: u8,
    ) -> ProgramResult {
        let acc_market = &ctx.accounts.market.to_account_info();
        let acc_state = &ctx.accounts.state.to_account_info();
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_manager = &ctx.accounts.manager.to_account_info();
        let acc_mkt_mint = &ctx.accounts.mkt_mint.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_mint = &ctx.accounts.prc_mint.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();

        // Verify market agent
        let acc_agent_expected = Pubkey::create_program_address(&[acc_market.key.as_ref(), &[inp_agent_nonce]], ctx.program_id)
            .map_err(|_| ErrorCode::InvalidDerivedAccount)?;
        verify_matching_accounts(acc_agent.key, &acc_agent_expected, Some(String::from("Invalid market agent")))?;

        let spl_token: Pubkey = Pubkey::from_str(SPL_TOKEN_PK).unwrap();
        let asc_token: Pubkey = Pubkey::from_str(ASC_TOKEN_PK).unwrap();

        // Verify associated token (market)
        let derived_mkt_vault = Pubkey::create_program_address(
            &[&acc_agent.key.to_bytes(), &spl_token.to_bytes(), &acc_mkt_mint.key.to_bytes(), &[inp_mkt_vault_nonce]],
            &asc_token
        ).map_err(|_| ErrorCode::InvalidDerivedAccount)?;
        if derived_mkt_vault != *acc_mkt_vault.key {
            msg!("Invalid market token vault");
            return Err(ErrorCode::InvalidDerivedAccount.into());
        }

        // Verify associated token (pricing)
        let derived_prc_vault = Pubkey::create_program_address(
            &[&acc_agent.key.to_bytes(), &spl_token.to_bytes(), &acc_prc_mint.key.to_bytes(), &[inp_prc_vault_nonce]],
            &asc_token
        ).map_err(|_| ErrorCode::InvalidDerivedAccount)?;
        if derived_prc_vault != *acc_prc_vault.key {
            msg!("Invalid pricing token vault");
            return Err(ErrorCode::InvalidDerivedAccount.into());
        }

        // Create token vaults
        let acc_spl = &ctx.accounts.spl_token_prog.to_account_info();
        let acc_sys = &ctx.accounts.sys_prog.to_account_info();
        let acc_rent = &ctx.accounts.sys_rent.to_account_info();

        let instr1 = Instruction {
            program_id: asc_token,
            accounts: vec![
                AccountMeta::new(*acc_manager.key, true),
                AccountMeta::new(*acc_mkt_vault.key, false),
                AccountMeta::new_readonly(*acc_agent.key, false),
                AccountMeta::new_readonly(*acc_mkt_mint.key, false),
                AccountMeta::new_readonly(solana_program::system_program::id(), false),
                AccountMeta::new_readonly(spl_token, false),
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

        let instr2 = Instruction {
            program_id: asc_token,
            accounts: vec![
                AccountMeta::new(*acc_manager.key, true),
                AccountMeta::new(*acc_prc_vault.key, false),
                AccountMeta::new_readonly(*acc_agent.key, false),
                AccountMeta::new_readonly(*acc_prc_mint.key, false),
                AccountMeta::new_readonly(solana_program::system_program::id(), false),
                AccountMeta::new_readonly(spl_token, false),
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

        let acc_orders = &ctx.accounts.orders.to_account_info();
        let acc_settle1 = &ctx.accounts.settle_a.to_account_info();
        let acc_settle2 = &ctx.accounts.settle_b.to_account_info();

        let market = Market {
            active: true,
            order_fee: 0,
            state: *acc_state.key,
            agent: *acc_agent.key,
            agent_nonce: inp_agent_nonce,
            manager: *acc_manager.key,
            mkt_mint: *acc_mkt_mint.key,
            mkt_vault: *acc_mkt_vault.key,
            mkt_nonce: inp_mkt_vault_nonce,
            prc_mint: *acc_prc_mint.key,
            prc_vault: *acc_prc_vault.key,
            prc_nonce: inp_prc_vault_nonce,
            orders: *acc_orders.key,
            settle_0: *acc_settle1.key,
            settle_a: *acc_settle1.key,
            settle_b: *acc_settle2.key,
        };
        store_struct::<Market>(&market, acc_market)?;

        let state = MarketState {
            order_counter: 0,
            active_bid: 0,
            active_ask: 0,
            mkt_vault_balance: 0,
            mkt_order_balance: 0,
            prc_vault_balance: 0,
            prc_order_balance: 0,
            last_ts: 0,
        };
        store_struct::<MarketState>(&state, acc_state)?;

        let order_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let order_slab = SlabPageAlloc::new(order_data);
        order_slab.setup_page_table();
        order_slab.allocate::<CritMapHeader, AnyNode>(OrderDT::BidOrderMap as u16, MAX_ORDERS as usize).expect("Failed to allocate");
        order_slab.allocate::<CritMapHeader, AnyNode>(OrderDT::AskOrderMap as u16, MAX_ORDERS as usize).expect("Failed to allocate");
        order_slab.allocate::<SlabVec, Order>(OrderDT::BidOrder as u16, MAX_ORDERS as usize).expect("Failed to allocate");
        order_slab.allocate::<SlabVec, Order>(OrderDT::AskOrder as u16, MAX_ORDERS as usize).expect("Failed to allocate");

        let settle1_data: &mut[u8] = &mut acc_settle1.try_borrow_mut_data()?;
        let (settle1_top, settle1_pages) = mut_array_refs![settle1_data, size_of::<AccountsHeader>(); .. ;];
        let settle1_header: &mut [AccountsHeader] = cast_slice_mut(settle1_top);
        settle1_header[0] = AccountsHeader {
            market: *acc_market.key,
            prev: Pubkey::default(),
            next: *acc_settle2.key,
        };
        let settle1_slab = SlabPageAlloc::new(settle1_pages);
        settle1_slab.setup_page_table();
        settle1_slab.allocate::<CritMapHeader, AnyNode>(SettleDT::AccountMap as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");
        settle1_slab.allocate::<SlabVec, AccountEntry>(SettleDT::Account as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");

        let settle2_data: &mut[u8] = &mut acc_settle2.try_borrow_mut_data()?;
        let (settle2_top, settle2_pages) = mut_array_refs![settle2_data, size_of::<AccountsHeader>(); .. ;];
        let settle2_header: &mut [AccountsHeader] = cast_slice_mut(settle2_top);
        settle2_header[0] = AccountsHeader {
            market: *acc_market.key,
            prev: *acc_settle2.key,
            next: Pubkey::default(),
        };
        let settle2_slab = SlabPageAlloc::new(settle2_pages);
        settle2_slab.setup_page_table();
        settle2_slab.allocate::<CritMapHeader, AnyNode>(SettleDT::AccountMap as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");
        settle2_slab.allocate::<SlabVec, AccountEntry>(SettleDT::Account as u16, MAX_ACCOUNTS as usize).expect("Failed to allocate");

        msg!("Atellix: Account Entry Size: {}", size_of::<AccountEntry>());

        msg!("Atellix: Created AquaDEX market");

        Ok(())
    }

    pub fn limit_bid(ctx: Context<LimitOrder>,
        inp_quantity: u64,
        inp_price: u64,
        inp_post: bool,     // Post the order order to the orderbook, otherwise it must be filled immediately
        inp_fill: bool,     // Require orders that are not posted to be filled completely (ignored for posted orders)
    ) -> ProgramResult {
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

        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let s1 = verify_matching_accounts(&market.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into());
        }

        msg!("Atellix: Limit Order Bid - Qty: {} @ Price: {}", inp_quantity.to_string(), inp_price.to_string());

        let tokens_in = inp_price * inp_quantity;
        let state_upd = &mut ctx.accounts.state;
        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_add(tokens_in).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_add(tokens_in).ok_or(ProgramError::from(ErrorCode::Overflow))?;

        let orderbook_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let ob = SlabPageAlloc::new(orderbook_data);

        // Check if order can be filled
        let mut tokens_to_fill: u64 = inp_quantity;
        let mut tokens_filled: u64 = 0;
        let mut tokens_paid: u64 = 0;
        loop {
            let node_res = map_predicate_min(ob, DT::AskOrder, |sl, leaf| {
                let valid = if leaf.owner() == *acc_user.key { false } else { true }; // Prevent trades between the same account
                let order = sl.index::<Order>(OrderDT::AskOrder as u16, leaf.slot() as usize);
                msg!("Atellix: Found Order[{}] - Owner: {} Qty: {} Price: {} Valid: {}",
                    leaf.slot().to_string(), leaf.owner().to_string(), order.amount.to_string(), Order::price(leaf.key()).to_string(), valid.to_string()
                );
                valid
            });
            if node_res.is_none() {
                msg!("Atellix: No Matching Orders In Orderbook");
                break;
            }
            let posted_node = node_res.unwrap();
            let posted_order = ob.index::<Order>(OrderDT::AskOrder as u16, posted_node.slot() as usize);
            let posted_qty = posted_order.amount;
            let posted_price = Order::price(posted_node.key());
            msg!("Atellix: Matched Ask Order[{}] - Qty: {} @ Price: {}", posted_node.slot().to_string(), posted_qty.to_string(), posted_price.to_string());
            if posted_price <= inp_price {
                // Fill order
                if posted_qty == tokens_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    let tokens_part = tokens_to_fill.checked_mul(posted_price).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - Qty: {} @ Price: {}", tokens_part.to_string(), posted_price.to_string());
                    map_remove(ob, DT::AskOrder, posted_node.key());
                    Order::free_index(ob, DT::AskOrder, posted_node.slot());
                    log_settlement(state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    break;
                } else if posted_qty < tokens_to_fill {   // Match the entire order and continue
                    tokens_to_fill = tokens_to_fill.checked_sub(posted_qty).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    let tokens_part = posted_qty.checked_mul(posted_price).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - Qty: {} @ Price: {}", posted_qty.to_string(), posted_price.to_string());
                    map_remove(ob, DT::AskOrder, posted_node.key());
                    Order::free_index(ob, DT::AskOrder, posted_node.slot());
                    log_settlement(state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                } else if posted_qty > tokens_to_fill {   // Match part of the order
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    let tokens_part = tokens_to_fill.checked_mul(posted_price).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - Qty: {} @ Price: {}", tokens_to_fill.to_string(), posted_price.to_string());
                    let new_amount = posted_qty.checked_sub(tokens_to_fill).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    ob.index_mut::<Order>(OrderDT::AskOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                    log_settlement(state_upd, acc_settle1, acc_settle2, &posted_node.owner(), false, tokens_part)?;
                    break;
                }
            } else {
                // Best price beyond limit price
                break;
            }
        }

        let mut result = TradeResult { tokens_filled: tokens_filled, tokens_posted: 0, order_id: 0 };

        // Add order to orderbook if not filled
        let tokens_remaining = inp_quantity.checked_sub(tokens_filled).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        if tokens_remaining > 0 && inp_post {
            let order_id = Order::new_key(state_upd, Side::Bid, inp_price);
            let order_idx = Order::next_index(ob, DT::BidOrder)?;
            let order_node = LeafNode::new(order_id, order_idx, &acc_user.key);
            let order = Order { amount: tokens_remaining };
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
                        msg!("Atellix: Orderbook Full - Price Does Not Exceed Evicted Order");
                        return Err(ErrorCode::OrderbookFull.into());
                    }
                    let evict_amount: u64 = evict_order.amount();
                    msg!("Atellix: Evicting Order[{}] - Owner: {} Qty: {} Price: {}",
                        evict_node.slot().to_string(),
                        evict_node.owner().to_string(),
                        evict_order.amount.to_string(),
                        Order::price(evict_node.key()).to_string(),
                    );
                    let evict_total = evict_amount.checked_mul(Order::price(evict_node.key())).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    log_settlement(state_upd, acc_settle1, acc_settle2, &evict_node.owner(), false, evict_total)?;
                    map_remove(ob, DT::BidOrder, evict_node.key());
                    Order::free_index(ob, DT::BidOrder, evict_node.slot());
                    eviction_count = eviction_count + 1;
                } else {
                    *ob.index_mut::<Order>(OrderDT::BidOrder.into(), order_idx as usize) = order;
                    break;
                }
            }
            let tokens_part = tokens_remaining.checked_mul(inp_price).ok_or(ProgramError::from(ErrorCode::Overflow))?;
            tokens_paid = tokens_paid.checked_add(tokens_part).ok_or(ProgramError::from(ErrorCode::Overflow))?;
            result.set_tokens_posted(tokens_remaining);
            result.set_order_id(order_id);
            msg!("Atellix: Posted Order[{}] - Qty: {} @ Price: {}", order_idx.to_string(), tokens_remaining.to_string(), inp_price.to_string());
        }
        store_struct::<TradeResult>(&result, acc_result)?;
        let discount = tokens_in.checked_sub(tokens_paid).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        msg!("Atellix: Discount: {}", discount.to_string());
        let total_cost = tokens_in.checked_sub(discount).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_sub(discount).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        state_upd.prc_order_balance = state_upd.prc_order_balance.checked_sub(discount).ok_or(ProgramError::from(ErrorCode::Overflow))?;

        // TODO: Pay for settlement log space

        // Send tokens to the vault
        let in_accounts = Transfer {
            from: ctx.accounts.user_prc_token.to_account_info(),
            to: ctx.accounts.prc_vault.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let cpi_prog = ctx.accounts.spl_token_prog.clone();
        let in_ctx = CpiContext::new(cpi_prog, in_accounts);
        /*msg!("Atellix: Pricing Token Vault Deposit: {}", tokens_in.to_string());
        msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
            state_upd.prc_vault_balance,
            state_upd.prc_order_balance,
        );*/
        token::transfer(in_ctx, total_cost)?;

        if tokens_filled > 0 {
            // Withdraw tokens from the vault
            state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_sub(tokens_filled).ok_or(ProgramError::from(ErrorCode::Overflow))?;
            state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_sub(tokens_filled).ok_or(ProgramError::from(ErrorCode::Overflow))?;

            let seeds = &[
                ctx.accounts.market.to_account_info().key.as_ref(),
                &[market.agent_nonce],
            ];
            let signer = &[&seeds[..]];
            let in_accounts = Transfer {
                from: ctx.accounts.mkt_vault.to_account_info(),
                to: ctx.accounts.user_mkt_token.to_account_info(),
                authority: ctx.accounts.agent.to_account_info(),
            };
            let cpi_prog = ctx.accounts.spl_token_prog.clone();
            let in_ctx = CpiContext::new_with_signer(cpi_prog, in_accounts, signer);
            /*msg!("Atellix: Market Token Vault Withdraw: {}", tokens_filled.to_string());
            msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
                state_upd.mkt_vault_balance,
                state_upd.mkt_order_balance,
            );*/
            token::transfer(in_ctx, tokens_filled)?;
        }
    
        Ok(())
    }

    pub fn limit_ask(ctx: Context<LimitOrder>,
        inp_quantity: u64,
        inp_price: u64,
        inp_post: bool,     // Post the order order to the orderbook, otherwise it must be filled immediately
        inp_fill: bool,     // Require orders that are not posted to be filled completely (ignored for posted orders)
    ) -> ProgramResult {
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

        verify_matching_accounts(&market.state, &market_state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;
        verify_matching_accounts(&market.orders, &acc_orders.key, Some(String::from("Invalid orderbook")))?;

        let s1 = verify_matching_accounts(&market.settle_a, &acc_settle1.key, Some(String::from("Settlement log 1")));
        let s2 = verify_matching_accounts(&market.settle_b, &acc_settle2.key, Some(String::from("Settlement log 2")));
        if s1.is_err() || s2.is_err() {
            // This is expected to happen sometimes due to a race condition between settlment log rollovers and new orders
            // Reload the current "market" account with the latest settlement log accounts and retry the transaction
            msg!("Please update market data and retry");
            return Err(ErrorCode::RetrySettlementAccount.into()); 
        }

        msg!("Atellix: Limit Order Ask - Qty: {} @ Price: {}", inp_quantity.to_string(), inp_price.to_string());

        let tokens_in = inp_quantity;
        let state_upd = &mut ctx.accounts.state;
        state_upd.mkt_vault_balance = state_upd.mkt_vault_balance.checked_add(inp_quantity).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        state_upd.mkt_order_balance = state_upd.mkt_order_balance.checked_add(inp_quantity).ok_or(ProgramError::from(ErrorCode::Overflow))?;

        let orderbook_data: &mut[u8] = &mut acc_orders.try_borrow_mut_data()?;
        let ob = SlabPageAlloc::new(orderbook_data);

        // Check if order can be filled
        let mut tokens_to_fill: u64 = inp_quantity;
        let mut tokens_filled: u64 = 0;
        let mut tokens_received: u64 = 0;
        loop {
            let node_res = map_predicate_max(ob, DT::BidOrder, |sl, leaf| {
                let valid = if leaf.owner() == *acc_user.key { false } else { true }; // Prevent trades between the same account
                let order = sl.index::<Order>(OrderDT::BidOrder as u16, leaf.slot() as usize);
                msg!("Atellix: Found Order[{}] - Owner: {} Qty: {} Price: {} Valid: {}",
                    leaf.slot().to_string(), leaf.owner().to_string(), order.amount.to_string(), Order::price(leaf.key()).to_string(), valid.to_string()
                );
                valid
            });
            if node_res.is_none() {
                msg!("Atellix: No Matching Orders In Orderbook");
                break;
            }
            let posted_node = node_res.unwrap();
            let posted_order = ob.index::<Order>(OrderDT::BidOrder as u16, posted_node.slot() as usize);
            let posted_qty = posted_order.amount;
            let posted_price = Order::price(posted_node.key());
            msg!("Atellix: Matched Bid Order[{}] - Qty: {} @ Price: {}", posted_node.slot().to_string(), posted_qty.to_string(), posted_price.to_string());
            if posted_price >= inp_price {
                // Fill order
                if posted_qty == tokens_to_fill {         // Match the entire order exactly
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    let tokens_part = tokens_to_fill.checked_mul(posted_price).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - Qty: {} @ Price: {}", tokens_part.to_string(), posted_price.to_string());
                    map_remove(ob, DT::BidOrder, posted_node.key());
                    Order::free_index(ob, DT::BidOrder, posted_node.slot());
                    log_settlement(state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, tokens_to_fill)?;
                    break;
                } else if posted_qty < tokens_to_fill {   // Match the entire order and continue
                    tokens_to_fill = tokens_to_fill.checked_sub(posted_qty).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_filled = tokens_filled.checked_add(posted_qty).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    let tokens_part = posted_qty.checked_mul(posted_price).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - Qty: {} @ Price: {}", posted_qty.to_string(), posted_price.to_string());
                    map_remove(ob, DT::BidOrder, posted_node.key());
                    Order::free_index(ob, DT::BidOrder, posted_node.slot());
                    log_settlement(state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, posted_qty)?;
                } else if posted_qty > tokens_to_fill {   // Match part of the order
                    tokens_filled = tokens_filled.checked_add(tokens_to_fill).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    let tokens_part = tokens_to_fill.checked_mul(posted_price).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    tokens_received = tokens_received.checked_add(tokens_part).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    msg!("Atellix: Filling - Qty: {} @ Price: {}", tokens_to_fill.to_string(), posted_price.to_string());
                    let new_amount = posted_qty.checked_sub(tokens_to_fill).ok_or(ProgramError::from(ErrorCode::Overflow))?;
                    ob.index_mut::<Order>(OrderDT::BidOrder as u16, posted_node.slot() as usize).set_amount(new_amount);
                    log_settlement(state_upd, acc_settle1, acc_settle2, &posted_node.owner(), true, tokens_to_fill)?;
                    break;
                }
            } else {
                // Best price beyond limit price
                break;
            }
        }

        let mut result = TradeResult { tokens_filled: tokens_filled, tokens_posted: 0, order_id: 0 };

        // Add order to orderbook if not filled
        let tokens_remaining = inp_quantity.checked_sub(tokens_filled).ok_or(ProgramError::from(ErrorCode::Overflow))?;
        if tokens_remaining > 0 && inp_post {
            // Add order to orderbook
            let order_id = Order::new_key(state_upd, Side::Ask, inp_price);
            let order_idx = Order::next_index(ob, DT::AskOrder)?;
            let order_node = LeafNode::new(order_id, order_idx, &acc_user.key);
            let order = Order { amount: tokens_remaining };
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
                        msg!("Atellix: Orderbook Full - Price Is Not Below Evicted Order");
                        return Err(ErrorCode::OrderbookFull.into());
                    }
                    let evict_amount: u64 = evict_order.amount();
                    msg!("Atellix: Evicting Order[{}] - Owner: {} Qty: {} Price: {}",
                        evict_node.slot().to_string(),
                        evict_node.owner().to_string(),
                        evict_order.amount.to_string(),
                        Order::price(evict_node.key()).to_string(),
                    );
                    log_settlement(state_upd, acc_settle1, acc_settle2, &evict_node.owner(), true, evict_amount)?;
                    map_remove(ob, DT::AskOrder, evict_node.key());
                    Order::free_index(ob, DT::AskOrder, evict_node.slot());
                    eviction_count = eviction_count + 1;
                } else {
                    *ob.index_mut::<Order>(OrderDT::AskOrder.into(), order_idx as usize) = order;
                    break;
                }
            }
            result.set_tokens_posted(tokens_remaining);
            result.set_order_id(order_id);
            msg!("Atellix: Posted Order[{}] - Qty: {} @ Price: {}", order_idx.to_string(), inp_quantity.to_string(), inp_price.to_string());
        }
        store_struct::<TradeResult>(&result, acc_result)?;

        // TODO: Pay for settlement log space

        // Send tokens to the vault
        let in_accounts = Transfer {
            from: ctx.accounts.user_mkt_token.to_account_info(),
            to: ctx.accounts.mkt_vault.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let cpi_prog = ctx.accounts.spl_token_prog.clone();
        let in_ctx = CpiContext::new(cpi_prog, in_accounts);
        /*msg!("Atellix: Market Token Vault Deposit: {}", inp_quantity.to_string());
        msg!("Atellix: Market Token Vault Balance: {} (Orderbook: {})",
            state_upd.mkt_vault_balance,
            state_upd.mkt_order_balance,
        );*/
        token::transfer(in_ctx, inp_quantity)?;

        if tokens_filled > 0 {
            // Withdraw tokens from the vault
            state_upd.prc_vault_balance = state_upd.prc_vault_balance.checked_sub(tokens_received).ok_or(ProgramError::from(ErrorCode::Overflow))?;
            state_upd.prc_order_balance = state_upd.prc_order_balance.checked_sub(tokens_received).ok_or(ProgramError::from(ErrorCode::Overflow))?;

            let seeds = &[
                ctx.accounts.market.to_account_info().key.as_ref(),
                &[market.agent_nonce],
            ];
            let signer = &[&seeds[..]];
            let in_accounts = Transfer {
                from: ctx.accounts.prc_vault.to_account_info(),
                to: ctx.accounts.user_prc_token.to_account_info(),
                authority: ctx.accounts.agent.to_account_info(),
            };
            let cpi_prog = ctx.accounts.spl_token_prog.clone();
            let in_ctx = CpiContext::new_with_signer(cpi_prog, in_accounts, signer);
            /*msg!("Atellix: Pricing Token Vault Withdraw: {}", tokens_filled.to_string());
            msg!("Atellix: Pricing Token Vault Balance: {} (Orderbook: {})",
                state_upd.prc_vault_balance,
                state_upd.prc_order_balance,
            );*/
            token::transfer(in_ctx, tokens_received)?;
        }
    
        Ok(())
    }

    /*pub fn cancel_order(ctx: Context<CancelOrder>,
        inp_param: u64,
    ) -> ProgramResult {
        Ok(())
    }*/

    pub fn withdraw(ctx: Context<Withdraw>) -> ProgramResult {
        let market = &ctx.accounts.market;
        let state = &mut ctx.accounts.state;
        let acc_agent = &ctx.accounts.agent.to_account_info();
        let acc_user = &ctx.accounts.user.to_account_info();
        let acc_mkt_vault = &ctx.accounts.mkt_vault.to_account_info();
        let acc_prc_vault = &ctx.accounts.prc_vault.to_account_info();
        let acc_settle = &ctx.accounts.settle.to_account_info();
        let acc_result = &ctx.accounts.result.to_account_info();

        // Verify 
        verify_matching_accounts(&market.state, &state.key(), Some(String::from("Invalid market state")))?;
        verify_matching_accounts(&market.agent, &acc_agent.key, Some(String::from("Invalid market agent")))?;
        verify_matching_accounts(&market.mkt_vault, &acc_mkt_vault.key, Some(String::from("Invalid market token vault")))?;
        verify_matching_accounts(&market.prc_vault, &acc_prc_vault.key, Some(String::from("Invalid pricing token vault")))?;

        let owner_key: u128 = CritMap::bytes_hash(acc_user.key.as_ref());
        let log_data: &mut[u8] = &mut acc_settle.try_borrow_mut_data()?;
        let (header, page_table) = mut_array_refs![log_data, size_of::<AccountsHeader>(); .. ;];
        let settle_header: &mut [AccountsHeader] = cast_slice_mut(header);
        verify_matching_accounts(&settle_header[0].market, &market.key(), Some(String::from("Invalid market")))?;
        let sl = SlabPageAlloc::new(page_table);
        let has_item = map_get(sl, DT::Account, owner_key);
        if has_item.is_some() {
            let log_node = has_item.unwrap();
            let log_entry = sl.index::<AccountEntry>(SettleDT::Account as u16, log_node.slot() as usize);
            let seeds = &[
                ctx.accounts.market.to_account_info().key.as_ref(),
                &[market.agent_nonce],
            ];
            let signer = &[&seeds[..]];
            let mut result = WithdrawResult { mkt_tokens: 0, prc_tokens: 0 };
            if log_entry.mkt_token_balance() > 0 {
                result.set_mkt_tokens(log_entry.mkt_token_balance());
                let in_accounts = Transfer {
                    from: ctx.accounts.mkt_vault.to_account_info(),
                    to: ctx.accounts.user_mkt_token.to_account_info(),
                    authority: ctx.accounts.agent.to_account_info(),
                };
                let cpi_prog = ctx.accounts.spl_token_prog.clone();
                let in_ctx = CpiContext::new_with_signer(cpi_prog, in_accounts, signer);
                token::transfer(in_ctx, log_entry.mkt_token_balance())?;
                state.mkt_vault_balance = state.mkt_vault_balance.checked_sub(log_entry.mkt_token_balance())
                    .ok_or(ProgramError::from(ErrorCode::Overflow))?;
            }
            if log_entry.prc_token_balance() > 0 {
                result.set_prc_tokens(log_entry.prc_token_balance());
                let in_accounts = Transfer {
                    from: ctx.accounts.prc_vault.to_account_info(),
                    to: ctx.accounts.user_prc_token.to_account_info(),
                    authority: ctx.accounts.agent.to_account_info(),
                };
                let cpi_prog = ctx.accounts.spl_token_prog.clone();
                let in_ctx = CpiContext::new_with_signer(cpi_prog, in_accounts, signer);
                token::transfer(in_ctx, log_entry.prc_token_balance())?;
                state.prc_vault_balance = state.prc_vault_balance.checked_sub(log_entry.prc_token_balance())
                    .ok_or(ProgramError::from(ErrorCode::Overflow))?;
            }
            // Remove log entry
            map_remove(sl, DT::Account, log_node.key());
            AccountEntry::free_index(sl, DT::Account, log_node.slot());
            // Write result
            store_struct::<WithdrawResult>(&result, acc_result)?;
        } else {
            msg!("Account not found");
            return Err(ErrorCode::AccountNotFound.into());
        }

        Ok(())
    }
}

#[derive(Accounts)]
pub struct CreateMarket<'info> {
    #[account(mut)]
    pub market: AccountInfo<'info>,
    #[account(mut)]
    pub state: AccountInfo<'info>,
    pub agent: AccountInfo<'info>,
    #[account(mut, signer)]
    pub manager: AccountInfo<'info>,
    pub mkt_mint: AccountInfo<'info>,
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    pub prc_mint: AccountInfo<'info>,
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    #[account(mut)]
    pub orders: AccountInfo<'info>,
    #[account(mut)]
    pub settle_a: AccountInfo<'info>,
    #[account(mut)]
    pub settle_b: AccountInfo<'info>,
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
    pub asc_token_prog: AccountInfo<'info>,
    #[account(address = system_program::ID)]
    pub sys_prog: AccountInfo<'info>,
    #[account(address = sysvar::rent::ID)]
    pub sys_rent: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct LimitOrder<'info> {
    #[account(mut)]
    pub market: ProgramAccount<'info, Market>,
    #[account(mut)]
    pub state: ProgramAccount<'info, MarketState>,
    pub agent: AccountInfo<'info>,
    #[account(mut, signer)]
    pub user: AccountInfo<'info>,
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>, // Deposit market tokens for "Ask" orders
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>, // Withdraw pricing tokens if the order is filled or partially filled
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    #[account(mut)]
    pub orders: AccountInfo<'info>,
    #[account(mut)]
    pub settle_a: AccountInfo<'info>,
    #[account(mut)]
    pub settle_b: AccountInfo<'info>,
    #[account(mut)]
    pub result: AccountInfo<'info>,
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut)]
    pub market: ProgramAccount<'info, Market>,
    #[account(mut)]
    pub state: ProgramAccount<'info, MarketState>,
    pub agent: AccountInfo<'info>,
    #[account(mut, signer)]
    pub user: AccountInfo<'info>,
    #[account(mut)]
    pub user_mkt_token: AccountInfo<'info>,
    #[account(mut)]
    pub user_prc_token: AccountInfo<'info>,
    #[account(mut)]
    pub mkt_vault: AccountInfo<'info>,
    #[account(mut)]
    pub prc_vault: AccountInfo<'info>,
    #[account(mut)]
    pub settle: AccountInfo<'info>,
    #[account(mut)]
    pub result: AccountInfo<'info>,
    #[account(address = token::ID)]
    pub spl_token_prog: AccountInfo<'info>,
}

#[account]
pub struct Market {
    pub active: bool,                   // Active flag
    pub order_fee: u64,                 // Fee to reserve space in a settlement log when an order is filled or evicted
    pub state: Pubkey,                  // Market statistics (frequently updated market details)
    pub agent: Pubkey,                  // Program derived address for signing transfers
    pub agent_nonce: u8,                // Agent nonce
    pub manager: Pubkey,                // Market manager
    pub mkt_mint: Pubkey,               // Token mint for market tokens (Token A)
    pub mkt_vault: Pubkey,              // Vault for Token A (an associated token account controlled by this program)
    pub mkt_nonce: u8,                  // Vault nonce for Token A
    pub prc_mint: Pubkey,               // Token mint for pricing tokens (Token B)
    pub prc_vault: Pubkey,              // Vault for Token B
    pub prc_nonce: u8,                  // Vault nonce for Token B
    pub orders: Pubkey,                 // Orderbook Bid/Ask entries
    pub settle_0: Pubkey,               // The start of the settlement log
    pub settle_a: Pubkey,               // Settlement log 1 (the active log)
    pub settle_b: Pubkey,               // Settlement log 2 (the next log)
}

#[account]
pub struct MarketState {
    pub order_counter: u64,             // Order index for Critmap ids (lower 64 bits)
    pub active_bid: u64,                // Active bid orders in the orderbook
    pub active_ask: u64,                // Active ask orders in the orderbook
    pub mkt_vault_balance: u64,         // Token A vault total balance (including tokens available to withdraw)
    pub mkt_order_balance: u64,         // Token A order balance (tokens in vault available to trade)
    pub prc_vault_balance: u64,         // Token B vault total balance
    pub prc_order_balance: u64,         // Token B order balance
    pub last_ts: i64,                   // Timestamp of last event
}

#[account]
pub struct TradeResult {
    pub tokens_filled: u64,             // Received tokens
    pub tokens_posted: u64,             // Posted tokens
    pub order_id: u128,                 // Order ID
}

impl TradeResult {
    pub fn set_tokens_filled(&mut self, new_amount: u64) {
        self.tokens_filled = new_amount;
    }

    pub fn set_tokens_posted(&mut self, new_amount: u64) {
        self.tokens_posted = new_amount;
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

#[error]
pub enum ErrorCode {
    #[msg("Account not found")]
    AccountNotFound,
    #[msg("Invalid parameters")]
    InvalidParameters,
    #[msg("Invalid account")]
    InvalidAccount,
    #[msg("Invalid derived account")]
    InvalidDerivedAccount,
    #[msg("Internal error")]
    InternalError,
    #[msg("External error")]
    ExternalError,
    #[msg("Orderbook full")]
    OrderbookFull,
    #[msg("Settlement log account does not match market, please update market data and retry")]
    RetrySettlementAccount,
    #[msg("Overflow")]
    Overflow,
}

