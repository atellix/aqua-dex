use std::{ io::Cursor, string::String, str::FromStr };
use bytemuck::{ Pod, Zeroable };
use byte_slice_cast::*;
use num_enum::TryFromPrimitive;
use anchor_lang::prelude::*;
use anchor_spl::token::{ self, Transfer };
use solana_program::{
    sysvar, system_instruction, system_program,
    program::{ invoke, invoke_signed },
    account_info::AccountInfo,
    instruction::{ AccountMeta, Instruction }
};

pub mod slab_alloc;
use crate::slab_alloc::{ SlabPageAlloc, CritMapHeader, CritMap, AnyNode, LeafNode, SlabVec };

pub const MAX_ORDERS: u32 = 128;    // Max orders on each side of the orderbook
pub const MAX_ACCOUNTS: u32 = 256;  // Max number of accounts per settlement data file

#[repr(u8)]
#[derive(PartialEq, Debug, Eq, Copy, Clone)]
pub enum Side {
    Bid = 0,
    Ask = 1,
}

#[repr(u16)]
#[derive(PartialEq, Debug, Eq, Copy, Clone)]
pub enum DT { // All data types
    BidOrders,
    AskOrders,
    AccountMap,
    Account,
}

#[repr(u16)]
#[derive(PartialEq, Debug, Eq, Copy, Clone)]
pub enum OrderDT {          // Orders data types
    BidOrders,              // CritMap - bids side of the orderbook
    AskOrders,              // CritMap - asks side of the orderbook
}

#[repr(u16)]
#[derive(PartialEq, Debug, Eq, Copy, Clone)]
pub enum SettleDT {         // Account settlement data types
    AccountMap,             // CritMap - settled account balances (cleared trades and evicted orders)
    Account,                // SlabVec - details of settled transactions
}

#[inline]
fn index_datatype(data_type: DT) -> u16 {
    match data_type {
        DT::Account => SettleDT::Account as u16,
        _ => { panic!("Invalid datatype") },
    }
}

#[inline]
fn map_datatype(data_type: DT) -> u16 {
    match data_type {
        DT::BidOrders  => DT::BidOrders as u16,
        DT::AskOrders  => DT::AskOrders as u16,
        DT::Account    => SettleDT::AccountMap as u16,
        _ => { panic!("Invalid datatype") },
    }
}

#[inline]
fn map_len(data_type: DT) -> u32 {
    match data_type {
        DT::BidOrders  => MAX_ORDERS,
        DT::AskOrders  => MAX_ORDERS,
        DT::AccountMap => MAX_ACCOUNTS,
        _ => 0,
    }
}

#[inline]
fn map_get(pt: &mut SlabPageAlloc, data_type: DT, key: u128) -> Option<u32> {
    let cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let rf = cm.get_key(key);
    match rf {
        None => None,
        Some(res) => Some(res.data()),
    }
}

#[inline]
fn map_set(pt: &mut SlabPageAlloc, data_type: DT, key: u128, data: u32) {
    let mut cm = CritMap { slab: pt, type_id: map_datatype(data_type), capacity: map_len(data_type) };
    let node = LeafNode::new(key, data);
    cm.insert_leaf(&node).expect("Failed to insert leaf");
}

#[inline]
fn next_index(pt: &mut SlabPageAlloc, data_type: DT) -> u32 {
    let svec = pt.header_mut::<SlabVec>(index_datatype(data_type));
    svec.next_index()
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

#[program]
pub mod aqua_dex {
    use super::*;

    pub fn create_market(ctx: Context<CreateMarket>,
        inp_param: u64,
    ) -> ProgramResult {
        Ok(())
    }

    /*pub fn limit_bid(ctx: Context<LimitOrder>,
        inp_param: u64,
    ) -> ProgramResult {
        Ok(())
    }*/

    /*pub fn cancel_order(ctx: Context<CancelOrder>,
        inp_param: u64,
    ) -> ProgramResult {
        Ok(())
    }*/

    /*pub fn withdraw(ctx: Context<Withdraw>,
        inp_param: u64,
    ) -> ProgramResult {
        Ok(())
    }*/
}

#[derive(Accounts)]
pub struct CreateMarket<'info> {
    #[account(mut)]
    pub market: AccountInfo<'info>,
    #[account(mut)]
    pub state: AccountInfo<'info>,
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
}

#[account]
pub struct Market {
    pub active: bool,                   // Active flag
    pub order_fee: u64,                 // Fee to reserve space in a settlement log when an order is filled or evicted
    pub state: Pubkey,                  // Market statistics (frequently updated market details)
    pub mkt_mint: Pubkey,               // Token mint for market tokens (Token A)
    pub mkt_vault: Pubkey,              // Vault for Token A (an associated token account controlled by this program)
    pub prc_mint: Pubkey,               // Token mint for pricing tokens (Token B)
    pub prc_vault: Pubkey,              // Vault for Token B
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
    pub prc_order_balanae: u64,         // Token B order balance
    pub last_ts: i64,                   // Timestamp of last event
}

#[error]
pub enum ErrorCode {
    #[msg("Access denied")]
    AccessDenied,
    #[msg("Invalid parameters")]
    InvalidParameters,
    #[msg("Invalid account")]
    InvalidAccount,
    #[msg("Invalid derived account")]
    InvalidDerivedAccount,
    #[msg("Overflow")]
    Overflow,
}

