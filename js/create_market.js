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

const provider = anchor.Provider.env()
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

async function programAddress(inputs, program = swapContractPK) {
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
    var res = await exec('./create_mint.sh')
    return res.stdout
}

async function main() {
    var mint1 = await createTokenMint()
    var mint2 = await createTokenMint()
    console.log("Mints: " + mint1 + " " + mint2)
    tokenMint1 = new PublicKey(mint1)
    tokenMint2 = new PublicKey(mint2)

    var writeData = {}
    writeData['tokenMint1'] = tokenMint1.toString()
    writeData['tokenMint2'] = tokenMint2.toString()

    market = anchor.web3.Keypair.generate()
    writeData['market'] = market.publicKey.toString()

    marketState = anchor.web3.Keypair.generate()
    writeData['marketState'] = marketState.publicKey.toString()

    var tx = new anchor.web3.Transaction()
    tx.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: market.publicKey,
        space: aquadex.account.market.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.market.size),
        programId: aquadexPK,
    }))
    tx.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: marketState.publicKey,
        space: aquadex.account.marketState.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.marketState.size),
        programId: aquadexPK,
    }))
    await provider.send(tx, [market, marketState])

        console.log('Initialize')
        await swapContract.rpc.initialize(
            new anchor.BN(rootBytes),
            new anchor.BN(rootRent),
            {
                accounts: {
                    program: swapContractPK,
                    programAdmin: provider.wallet.publicKey,
                    programData: new PublicKey(programData),
                    rootData: new PublicKey(rootData.pubkey),
                    authData: authData.publicKey,
                    systemProgram: SystemProgram.programId,
                }
            }
        )
        try {
            await fs.writeFile('market.json', JSON.stringify(writeData, null, 4))
        } catch (error) {
            console.log("File Error: " + error)
        }
    } else {
        var spjs
        try {
            spjs = await fs.readFile('market.json')
        } catch (error) {
            console.error('File Error: ', error)
        }
        const marketCache = JSON.parse(spjs.toString())
        //authDataPK = new PublicKey(swapCache.swapContractRBAC)
        //swapAdmin1 = importSecretKey(swapCache.swapAdmin1_secret)
    }

    const rootData = await programAddress([swapContractPK.toBuffer()])
    const tokData1 = await associatedTokenAddress(new PublicKey(rootData.pubkey), tokenMint1)
    const tokData2 = await associatedTokenAddress(new PublicKey(rootData.pubkey), tokenMint2)

    if (true) {
        console.log('Fund Swap Deposit Admin')
        var tx = new anchor.web3.Transaction()
        tx.add(
            anchor.web3.SystemProgram.transfer({
                fromPubkey: provider.wallet.publicKey,
                toPubkey: swapDeposit1.publicKey,
                lamports: (tkiRent + await provider.connection.getMinimumBalanceForRentExemption(165)) * 2,
            })
        )
        await provider.send(tx)
    }

    const tkiData1 = await programAddress([tokenMint1.toBuffer()])
    const tkiData2 = await programAddress([tokenMint2.toBuffer()])

    console.log('Approve Token 1: ' + tokenMint1.toString())
    await swapContract.rpc.approveToken(
        rootData.nonce,
        tkiData1.nonce,
        tokData1.nonce,
        new anchor.BN(tkiRent),
        new anchor.BN(tkiBytes),
        4,
        {
            accounts: {
                rootData: new PublicKey(rootData.pubkey),
                authData: authDataPK,
                swapAdmin: swapDeposit1.publicKey,
                swapToken: new PublicKey(tokData1.pubkey),
                tokenMint: tokenMint1,
                tokenInfo: new PublicKey(tkiData1.pubkey),
                tokenProgram: TOKEN_PROGRAM_ID,
                ascProgram: SPL_ASSOCIATED_TOKEN,
                systemProgram: SystemProgram.programId,
                systemRent: SYSVAR_RENT_PUBKEY,
            },
            signers: [swapDeposit1],
        }
    )

    console.log('Approve Token 2: ' + tokenMint2.toString())
    await swapContract.rpc.approveToken(
        rootData.nonce,
        tkiData2.nonce,
        tokData2.nonce,
        new anchor.BN(tkiRent),
        new anchor.BN(tkiBytes),
        4,
        {
            accounts: {
                rootData: new PublicKey(rootData.pubkey),
                authData: authDataPK,
                swapAdmin: swapDeposit1.publicKey,
                swapToken: new PublicKey(tokData2.pubkey),
                tokenMint: tokenMint2,
                tokenInfo: new PublicKey(tkiData2.pubkey),
                tokenProgram: TOKEN_PROGRAM_ID,
                ascProgram: SPL_ASSOCIATED_TOKEN,
                systemProgram: SystemProgram.programId,
                systemRent: SYSVAR_RENT_PUBKEY,
            },
            signers: [swapDeposit1],
        }
    )

    console.log('Deposit 1: ' + tokData1.pubkey)
    await swapContract.rpc.deposit(
        rootData.nonce,
        tkiData1.nonce,
        tokData1.nonce,
        true,
        new anchor.BN(1000000000000000),
        {
            accounts: {
                rootData: new PublicKey(rootData.pubkey),
                authData: authDataPK,
                swapAdmin: swapDeposit1.publicKey,
                swapToken: new PublicKey(tokData1.pubkey),
                tokenAdmin: provider.wallet.publicKey,
                tokenMint: tokenMint1,
                tokenInfo: new PublicKey(tkiData1.pubkey),
                tokenProgram: TOKEN_PROGRAM_ID,
            },
            signers: [swapDeposit1],
        }
    )

    console.log('Deposit 2: ' + tokData2.pubkey)
    await swapContract.rpc.deposit(
        rootData.nonce,
        tkiData2.nonce,
        tokData2.nonce,
        true,
        new anchor.BN(1000000000000000),
        {
            accounts: {
                rootData: new PublicKey(rootData.pubkey),
                authData: authDataPK,
                swapAdmin: swapDeposit1.publicKey,
                swapToken: new PublicKey(tokData2.pubkey),
                tokenAdmin: provider.wallet.publicKey,
                tokenMint: tokenMint2,
                tokenInfo: new PublicKey(tkiData2.pubkey),
                tokenProgram: TOKEN_PROGRAM_ID,
            },
            signers: [swapDeposit1],
        }
    )

    console.log('Create Swap')
    feesAcct = anchor.web3.Keypair.generate()
    if (true) {
        const tx = new anchor.web3.Transaction()
        tx.add(
            anchor.web3.SystemProgram.createAccount({
                fromPubkey: provider.wallet.publicKey,
                newAccountPubkey: swapDataPK,
                space: swapBytes,
                lamports: swapRent,
                programId: swapContractPK,
            })
        )
        await provider.send(tx, [swapData])
    }
    /*console.log({
        rootData: new PublicKey(rootData.pubkey).toString(),
        authData: authDataPK.toString(),
        swapAdmin: swapAdmin1.publicKey.toString(),
        swapData: swapDataPK.toString(),
        inbInfo: new PublicKey(tkiData1.pubkey).toString(),
        outInfo: new PublicKey(tkiData2.pubkey).toString(),
        feesAccount: feesAcct.publicKey.toString(),
    })*/
    await swapContract.rpc.createSwap(
        rootData.nonce,
        true, // use oracle
        false, // inverse oracle
        false, // oracle range check
        new anchor.BN(0), // range min
        new anchor.BN(0), // range max
        new anchor.BN(1), // swap rate
        new anchor.BN(1), // base rate
        false, // fees on inbound token
        0, // fees basis points
        {
            accounts: {
                rootData: new PublicKey(rootData.pubkey),
                authData: authDataPK,
                swapAdmin: swapAdmin1.publicKey,
                swapData: swapDataPK,
                inbInfo: new PublicKey(tkiData1.pubkey),
                outInfo: new PublicKey(tkiData2.pubkey),
                feesAccount: feesAcct.publicKey,
            },
            remainingAccounts: [
                { pubkey: oraclePK, isWritable: false, isSigner: false },
            ],
            signers: [swapAdmin1],
        }
    )

    /*console.log('Swap')
    const userToken1 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint1)
    const userToken2 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint2)
    console.log({
        rootData: new PublicKey(rootData.pubkey).toString(),
        authData: authDataPK.toString(),
        swapUser: provider.wallet.publicKey.toString(),
        swapData: swapDataPK.toString(),
        inbInfo: new PublicKey(tkiData1.pubkey).toString(),
        inbTokenSrc: new PublicKey(userToken1.pubkey).toString(),
        inbTokenDst: new PublicKey(tokData1.pubkey).toString(),
        inbMint: tokenMint1.toString(),
        outInfo: new PublicKey(tkiData2.pubkey).toString(),
        outTokenSrc: new PublicKey(tokData2.pubkey).toString(),
        outTokenDst: new PublicKey(userToken2.pubkey).toString(),
        outMint: tokenMint2.toString(),
    })
    await swapContract.rpc.swap(
        rootData.nonce,
        tokData1.nonce,
        tokData2.nonce,
        true, // True - Buy, False - Sell
        new anchor.BN(999 * 10000),
        {
            accounts: {
                rootData: new PublicKey(rootData.pubkey),
                authData: authDataPK,
                swapUser: provider.wallet.publicKey,
                swapData: swapDataPK,
                inbInfo: new PublicKey(tkiData1.pubkey),
                inbTokenSrc: new PublicKey(userToken1.pubkey),
                inbTokenDst: new PublicKey(tokData1.pubkey),
                outInfo: new PublicKey(tkiData2.pubkey),
                outTokenSrc: new PublicKey(tokData2.pubkey),
                outTokenDst: new PublicKey(userToken2.pubkey),
                tokenProgram: TOKEN_PROGRAM_ID,
                //feesAccount: feesAcct.publicKey,
            },
        }
    )*/
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
