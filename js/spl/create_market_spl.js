const { Buffer } = require('buffer')
const { DateTime } = require("luxon")
const { v4: uuidv4, parse: uuidparse } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID } = require('@solana/spl-token')
const { promisify } = require('util')
const exec = promisify(require('child_process').exec)
const fs = require('fs').promises
const base32 = require("base32.js")
const anchor = require('@project-serum/anchor')

const provider = anchor.AnchorProvider.env()
//const provider = anchor.Provider.local()
anchor.setProvider(provider)

const aquadex = anchor.workspace.AquaDex
const aquadexPK = aquadex.programId

const SPL_ASSOCIATED_TOKEN = new PublicKey('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL')
async function associatedTokenAddress(walletAddress, tokenMintAddress) {
    const addr = await PublicKey.findProgramAddress(
        [walletAddress.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), tokenMintAddress.toBuffer()],
        SPL_ASSOCIATED_TOKEN
    )
    const res = { 'pubkey': await addr[0].toString(), 'nonce': addr[1] }
    return res
}

async function programAddress(inputs, program = aquadexPK) {
    const addr = await PublicKey.findProgramAddress(inputs, program)
    const res = { 'pubkey': await addr[0].toString(), 'nonce': addr[1] }
    return res
}

function exportSecretKey(keyPair) {
    var enc = new base32.Encoder({ type: "crockford", lc: true })
    return enc.write(keyPair.secretKey).finalize()
}

function importSecretKey(keyStr) {
    var dec = new base32.Decoder({ type: "crockford" })
    var spec = dec.write(keyStr).finalize()
    return Keypair.fromSecretKey(new Uint8Array(spec))
}

async function createTokenMint() {
    var res = await exec('./new_token.sh')
    return res.stdout
}

async function main() {
    var mint1 = await createTokenMint()
    var mint2 = await createTokenMint()
    //var mint1 = 'So11111111111111111111111111111111111111112'
    //var mint2 = 'USDVXgXZcQWycX4PAu2CZbGaSG1Ft5rNjo4ARpoqw7w'
    console.log("Mints: " + mint1 + " " + mint2)
    const tokenMint1 = new PublicKey(mint1)
    const tokenMint2 = new PublicKey(mint2)

    var writeData = {}
    writeData['tokenMint1'] = tokenMint1.toString()
    writeData['tokenMint2'] = tokenMint2.toString()

    market = anchor.web3.Keypair.generate()
    marketPK = market.publicKey
    //marketPK = new PublicKey('DiugnZvAuabA21qhNWfRvAJT6cpx1xyXRTF3P4b4xyJw')
    marketState = anchor.web3.Keypair.generate()
    orders = anchor.web3.Keypair.generate()
    tradeLog = anchor.web3.Keypair.generate()
    settle1 = anchor.web3.Keypair.generate()
    settle2 = anchor.web3.Keypair.generate()
    writeData['market'] = marketPK.toString()
    writeData['marketState'] = marketState.publicKey.toString()
    writeData['orders'] = orders.publicKey.toString()
    writeData['tradeLog'] = tradeLog.publicKey.toString()
    writeData['settle1'] = settle1.publicKey.toString()
    writeData['settle2'] = settle2.publicKey.toString()

    // 2 + ((8 + 8 + 8 + (16 * 2)) * 4) = 226

    const ordersBytes = 226 + (16384 * 6)
    const ordersRent = await provider.connection.getMinimumBalanceForRentExemption(ordersBytes)

    const tradeLogBytes = 326 + (16384 * 1)
    const tradeLogRent = await provider.connection.getMinimumBalanceForRentExemption(tradeLogBytes)

    const settleBytes = 326 + (16384 * 6)
    const settleRent = await provider.connection.getMinimumBalanceForRentExemption(settleBytes)

    console.log("Orders Rent: " + ordersRent)
    console.log("TradeLog Rent: " + tradeLogRent)
    console.log("Settle Rent: " + settleRent)

    const accountId1 = uuidv4()
    const accountBuf1 = Buffer.from(uuidparse(accountId1).reverse())

    const marketAgent = await programAddress([marketPK.toBuffer()], aquadexPK)
    const marketAgentPK = new PublicKey(marketAgent.pubkey)
    const marketAdmin = await programAddress([marketPK.toBuffer(), Buffer.from('admin', 'utf8')], aquadexPK)
    const marketAdminPK = new PublicKey(marketAdmin.pubkey)
    const tokenVault1 = await associatedTokenAddress(marketAgentPK, tokenMint1)
    const tokenVault2 = await associatedTokenAddress(marketAgentPK, tokenMint2)

    console.log("Market Agent: " + marketAgent.pubkey)

    console.log("Token Vault: " + tokenVault1.pubkey)
    console.log("Token Vault UUID: " + accountId1)

    console.log("Market Size: " + aquadex.account.market.size)
    console.log("MarketState Size: " + aquadex.account.marketState.size)

    var ta = new anchor.web3.Transaction()
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: marketPK,
        space: aquadex.account.market.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.market.size),
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: marketState.publicKey,
        space: aquadex.account.marketState.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.marketState.size),
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: orders.publicKey,
        space: ordersBytes,
        lamports: ordersRent,
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: tradeLog.publicKey,
        space: tradeLogBytes,
        lamports: tradeLogRent,
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: settle1.publicKey,
        space: settleBytes,
        lamports: settleRent,
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: settle2.publicKey,
        space: settleBytes,
        lamports: settleRent,
        programId: aquadexPK,
    }))
    console.log(await provider.sendAndConfirm(ta, [market, marketState, orders, tradeLog, settle1, settle2]))

    var tx = new anchor.web3.Transaction()
    console.log({
        market: marketPK.toString(),
        admin: marketAdminPK.toString(),
        state: marketState.publicKey.toString(),
        tradeLog: tradeLog.publicKey.toString(),
        agent: new PublicKey(marketAgent.pubkey).toString(),
        manager: provider.wallet.publicKey.toString(),
        feeManager: provider.wallet.publicKey.toString(),
        vaultManager: provider.wallet.publicKey.toString(),
        mktMint: tokenMint1.toString(),
        mktVault: new PublicKey(tokenVault1.pubkey).toString(),
        prcMint: tokenMint2.toString(),
        prcVault: new PublicKey(tokenVault2.pubkey).toString(),
        orders: orders.publicKey.toString(),
        settleA: settle1.publicKey.toString(),
        settleB: settle2.publicKey.toString(),
        splTokenProg: TOKEN_PROGRAM_ID.toString(),
        ascTokenProg: SPL_ASSOCIATED_TOKEN.toString(),
        sysProg: SystemProgram.programId.toString(),
        sysRent: SYSVAR_RENT_PUBKEY.toString(),
    })
    tx.add(aquadex.instruction.createMarket(
        marketAgent.nonce,
        tokenVault1.nonce,
        tokenVault2.nonce,
        6,
        6,
        0,                                      // Mkt Mint Type - 0: SPL, 1: AST
        0,                                      // Prc Mint Type 
        false,                                  // Manager withdrawals (FALSE for trustless mode)
        false,                                  // Manager order cancel (FALSE for trustless mode)
        true,                                   // Expire enable
        new anchor.BN(1),                       // Min expire
        new anchor.BN(0),                       // Min quantity
        new anchor.BN(3600),                    // Taker fee (X / 10,000,000; or 1,000 = 1 BPS)
        new anchor.BN(2500),                    // Maker rebate (X / 10,000,000; or 1,000 = 1 BPS)
        new anchor.BN(0),                       // Log fee (reserve space in settlement log)
        new anchor.BN(0),                       // Log rebate (when closing settled position)
        new anchor.BN(0),                       // Log reimburse (for creating new settlement log accounts)
        new anchor.BN(0),                       // Mkt Token UUID (Security Tokens only)
        new anchor.BN(0),                       // Prc Token UUID (Security Tokens only)
        {
            accounts: {
                market: marketPK,
                admin: marketAdminPK,
                state: marketState.publicKey,
                tradeLog: tradeLog.publicKey,
                agent: new PublicKey(marketAgent.pubkey),
                manager: provider.wallet.publicKey,
                feeManager: provider.wallet.publicKey,
                vaultManager: provider.wallet.publicKey,
                mktMint: tokenMint1,
                mktVault: new PublicKey(tokenVault1.pubkey),
                prcMint: tokenMint2,
                prcVault: new PublicKey(tokenVault2.pubkey),
                orders: orders.publicKey,
                settleA: settle1.publicKey,
                settleB: settle2.publicKey,
                splTokenProg: TOKEN_PROGRAM_ID,
                ascTokenProg: SPL_ASSOCIATED_TOKEN,
                systemProgram: SystemProgram.programId,
                systemRent: SYSVAR_RENT_PUBKEY,
            },
        }
    ))

    console.log('Create Market')
    console.log(await provider.sendAndConfirm(tx))

    try {
        await fs.writeFile('market_4.json', JSON.stringify(writeData, null, 4))
    } catch (error) {
        console.log("File Error: " + error)
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
