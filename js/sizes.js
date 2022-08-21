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

async function main() {
    const ordersBytes = 130 + (16384 * 8)
    const ordersRent = await provider.connection.getMinimumBalanceForRentExemption(ordersBytes)

    const settleBytes = 130 + (16384 * 8)
    const settleRent = await provider.connection.getMinimumBalanceForRentExemption(settleBytes)

    console.log("Orderbook Rent: " + ordersRent)
    console.log("Settlement Log Rent: " + settleRent)
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
