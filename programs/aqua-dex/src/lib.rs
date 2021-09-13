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
pub const SPL_TOKEN_PK: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ASC_TOKEN_PK: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

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
        let acc_asc = &ctx.accounts.asc_token_prog.to_account_info();
        let acc_sys = &ctx.accounts.sys_prog.to_account_info();
        let acc_rent = &ctx.accounts.sys_rent.to_account_info();

        let instr = Instruction {
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
        let res = invoke(&instr, &[
            acc_manager.clone(), acc_mkt_vault.clone(), acc_agent.clone(), acc_mkt_mint.clone(),
            acc_spl.clone(), acc_sys.clone(), acc_rent.clone(),
        ]);
        if res.is_err() {
            msg!("Create associated token failed");
            return Err(ErrorCode::ExternalError.into());
        }

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
    #[account(mut)]
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

#[account]
pub struct Market {
    pub active: bool,                   // Active flag
    pub order_fee: u64,                 // Fee to reserve space in a settlement log when an order is filled or evicted
    pub state: Pubkey,                  // Market statistics (frequently updated market details)
    pub agent: Pubkey,                  // Market program derived address for signing transfers
    pub manager: Pubkey,                // Market manager
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

#[account]
pub struct MarketAgent {
    pub created: bool,
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
    #[msg("External error")]
    ExternalError,
    #[msg("Overflow")]
    Overflow,
}

