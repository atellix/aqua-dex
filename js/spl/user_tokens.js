const { Buffer } = require('buffer')
const { DateTime } = require("luxon")
const { v4: uuidv4, parse: uuidparse } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY, Transaction } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID, transfer, createSyncNativeInstruction, getOrCreateAssociatedTokenAccount } = require('@solana/spl-token')
const { promisify } = require('util')
const exec = promisify(require('child_process').exec)
const fs = require('fs').promises
const base32 = require("base32.js")
const anchor = require('@project-serum/anchor')

const provider = anchor.AnchorProvider.env()
anchor.setProvider(provider)

const SPL_TOKEN_BYTES = 165
const SPL_ASSOCIATED_TOKEN = new PublicKey('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL')
async function associatedTokenAddress(walletAddress, tokenMintAddress) {
    const addr = await PublicKey.findProgramAddress(
        [walletAddress.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), tokenMintAddress.toBuffer()],
        SPL_ASSOCIATED_TOKEN
    )
    const res = { 'pubkey': await addr[0].toString(), 'nonce': addr[1] }
    return res
}

const timer = ms => new Promise(res => setTimeout(res, ms))

function exportSecretKey(keyPair) {
    var enc = new base32.Encoder({ type: "crockford", lc: true })
    return enc.write(keyPair.secretKey).finalize()
}

function importSecretKey(keyStr) {
    var dec = new base32.Decoder({ type: "crockford" })
    var spec = dec.write(keyStr).finalize()
    return Keypair.fromSecretKey(new Uint8Array(spec))
}

async function main() {
    var mjs
    try {
        mjs = await fs.readFile('market_3.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(mjs.toString())
    const tokenMint1 = new PublicKey(mktData.tokenMint1) // Market token
    const tokenMint2 = new PublicKey(mktData.tokenMint2) // Pricing token

    var ujs
    try {
        ujs = await fs.readFile('user_data/user_list.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const users = JSON.parse(ujs.toString())
      
    for (var i = 0; i < users.length; i++) {
        user = users[i]
        userWallet = importSecretKey(user.secret)

        // Airdrop SOL
        if (false) {
            var res = await exec('solana airdrop 2 ' + userWallet.publicKey.toString())
            console.log(res.stdout)
            /*var airdropSig = await provider.connection.requestAirdrop(userWallet.publicKey, anchor.web3.LAMPORTS_PER_SOL * 2)
            var airdropConfirm = await provider.connection.confirmTransaction(airdropSig)
            console.log('User: ' + (i + 1) + ' PK: ' + user.pubkey + ' Airdrop Complete')
            console.log(airdropSig)*/
        }

        // Send SOL for running TXs
        if (false) {
            tx = new Transaction()
            tx.add(SystemProgram.transfer({
                fromPubkey: provider.wallet.publicKey, 
                lamports: anchor.web3.LAMPORTS_PER_SOL * 1,
                toPubkey: userWallet.publicKey,
            }))
            try {
                console.log('Transfer SOL: ' + await provider.sendAndConfirm(tx))
            } catch (error) {
                console.log('Transfer SOL: Error: ' + error)
            }
        }

        // Create associated tokens
        var userToken1
        var userToken2
        try {
            userToken1 = await getOrCreateAssociatedTokenAccount(provider.connection, userWallet, tokenMint1, userWallet.publicKey)
        } catch (error) {
            console.log('Create User Token 1: Error: ' + error)
        }
        try {
            userToken2 = await getOrCreateAssociatedTokenAccount(provider.connection, userWallet, tokenMint2, userWallet.publicKey)
        } catch (error) {
            console.log('Create User Token 2: Error: ' + error)
        }

        if (false) {
            tx2 = new Transaction()
            tx2.add(SystemProgram.transfer({
                fromPubkey: provider.wallet.publicKey, 
                lamports: anchor.web3.LAMPORTS_PER_SOL * 3,
                toPubkey: userToken1.address,
            }))
            tx2.add(createSyncNativeInstruction(userToken1.address))
            try {
                console.log('Transfer/Wrap SOL: ' + await provider.sendAndConfirm(tx2))
            } catch (error) {
                console.log('Transfer/Wrap SOL: Error: ' + error)
            }
        }

        if (true) {
            var ascToken1 = await associatedTokenAddress(userWallet.publicKey, tokenMint1)
            var srcToken1 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint1)
            try {
                console.log('Transfer Token A: ' + await transfer(
                    provider.connection,
                    provider.wallet.payer,
                    new PublicKey(srcToken1.pubkey),
                    new PublicKey(ascToken1.pubkey),
                    provider.wallet.payer,
                    1000000 * (10**6),
                ))
            } catch (error) {
                console.log('Transfer Token A: Error: ' + error)
            }
        }

        if (true) {
            var ascToken2 = await associatedTokenAddress(userWallet.publicKey, tokenMint2)
            var srcToken2 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint2)
            try {
                console.log('Transfer Token B: ' + await transfer(
                    provider.connection,
                    provider.wallet.payer,
                    new PublicKey(srcToken2.pubkey),
                    new PublicKey(ascToken2.pubkey),
                    provider.wallet.payer,
                    1000000 * (10**6),
                ))
            } catch (error) {
                console.log('Transfer Token B: Error: ' + error)
            }
        }

        //console.log("Asc Token 1: " + userToken1.address.toString())
        //console.log("Asc Token 2: " + userToken2.address.toString())
        console.log('User: ' + (i + 1) + ' PK: ' + user.pubkey)

        await timer(1000)
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
