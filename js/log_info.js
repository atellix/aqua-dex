const { Buffer } = require('buffer')
const { DateTime } = require("luxon")
const { v4: uuidv4, parse: uuidparse, stringify: uuidstringify } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID } = require('@solana/spl-token')
const { promisify } = require('util')
const exec = promisify(require('child_process').exec)
const fs = require('fs').promises
const base32 = require("base32.js")
const anchor = require('@project-serum/anchor')

const provider = anchor.AnchorProvider.env()
//const provider = anchor.AnchorProvider.local()
anchor.setProvider(provider)

const aquadex = anchor.workspace.AquaDex
const aquadexPK = aquadex.programId

function showData(spec) {
    var r = {}
    for (var i in spec) {
        if (typeof spec[i] === 'object' && spec[i].constructor.name === 'Object') {
            r[i] = showData(spec[i])
        } else if (typeof spec[i].toString !== 'undefined') {
            r[i] = spec[i].toString()
        }
    }
    return r
}

async function main() {
    var ndjs
    try {
        ndjs = await fs.readFile('market.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(ndjs.toString())
    //console.log(mktData)
    const statePK = new PublicKey(mktData.marketState)
    var ms = await aquadex.account.marketState.fetch(statePK)
    console.log('Market State')
    console.log(showData(ms))
    var log = ms.settleA
    console.log('View Log A: ' + log.toString())
    console.log(await aquadex.methods.logStatus().accounts({'settle': log}).view())
    log = ms.settleB
    console.log('View Log B: ' + log.toString())
    console.log(await aquadex.methods.logStatus().accounts({'settle': log}).view())
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
