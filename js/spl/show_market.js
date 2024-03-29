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
//const provider = anchor.Provider.local()
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
    var market = process.argv[2]
    //console.log(mktData)
    const marketPK = new PublicKey(market)
    const marketSpec = await aquadex.account.market.fetch(marketPK)
    const marketStatePK = marketSpec.state
    console.log('Market: ' + marketPK.toString())
    console.log(showData(marketSpec))
    console.log('Market State: ' + marketStatePK.toString())
    console.log(showData(await aquadex.account.marketState.fetch(marketStatePK)))
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
