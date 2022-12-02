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
const lo = require('buffer-layout')
const bigintConv = require('bigint-conversion')

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

function encodeOrderId(orderIdBuf) {
    const enc = new base32.Encoder({ type: "crockford", lc: true })
    var zflist = orderIdBuf.toJSON().data
    var zflen = 16 - zflist.length
    if (zflen > 0) {
        zfprefix = Array(zflen).fill(0)
        zflist = zfprefix.concat(zflist)
    }
    zflist.reverse()
    return enc.write(new Uint8Array(zflist)).finalize()
}

function decodeTradeLogVec(pageTableEntry, pages) {
    const headerSize = pageTableEntry['header_size']
    const offsetSize = pageTableEntry['offset_size']
    const stLogEntry = lo.struct([
        lo.blob(16, 'event_type'),
        lo.nu64('action_id'),
        lo.nu64('trade_id'),
        lo.blob(16, 'maker_order_id'),
        lo.u8('maker_filled'),
        lo.blob(32, 'maker'),
        lo.blob(32, 'taker'),
        lo.u8('taker_side'),
        lo.nu64('amount'),
        lo.nu64('price'),
        lo.ns64('ts'),
    ])
    const instPerPage = Math.floor((16384 - (headerSize + offsetSize)) / stLogEntry.span)
    const stSlabVec = lo.struct([
        lo.blob(offsetSize),
        lo.nu64('trade_count'),
        lo.nu64('entry_max'),
        lo.seq(stLogEntry, instPerPage, 'logs'),
    ])
    var totalPages = Math.floor(pageTableEntry['alloc_items'] / instPerPage)
    if ((pageTableEntry['alloc_items'] % instPerPage) !== 0) {
        totalPages = totalPages + 1
    }
    var vecPages = []
    for (var p = 0; p < totalPages; p++) {
        var pidx = pageTableEntry['alloc_pages'][p]
        vecPages.push(pages[pidx])
    }
    var logSpec = {
        'logs': [],
    }
    for (var i = 0; i < vecPages.length; i++) {
        var res = stSlabVec.decode(vecPages[i])
        if (i === 0) {
            logSpec['trade_count'] = res['trade_count']
            logSpec['entry_max'] = res['entry_max']
        }
        for (var logIdx = 0; logIdx < res['logs'].length; logIdx++) {
            var item = res['logs'][logIdx]
            if (item['trade_id'] === 0) {
                continue
            }
            item['event_type'] = (new anchor.BN(item['event_type'])).toString()
            item['maker_order_id'] = encodeOrderId(item['maker_order_id'])
            item['maker'] = new PublicKey(item['maker']).toString()
            item['taker'] = new PublicKey(item['taker']).toString()
            logSpec['logs'].push(item)
            if (logSpec['logs'].length === pageTableEntry['alloc_items']) {
                i = vecPages.length
                break
            }
        }
    }
    logSpec.logs = logSpec.logs.sort((a, b) => { return b.trade_id - a.trade_id })
    return logSpec
}

function decodeTradeLog(data) {
    const stTypedPage = lo.struct([
        lo.nu64('header_size'),
        lo.nu64('offset_size'),
        lo.nu64('alloc_items'),
        lo.seq(lo.u16('page_index'), 128, 'alloc_pages'), // TYPE_MAX_PAGES
    ]);
    const stSlabAlloc = lo.struct([
        lo.u16('top_unused_page'),
        lo.seq(stTypedPage, 16, 'type_page'), // TYPE_MAX
        lo.seq(lo.blob(16384), 4, 'pages'), // PAGE_SIZE
    ]);
    var res = stSlabAlloc.decode(data)
    //console.log(JSON.stringify(res['type_page']))
    var logVec = res['type_page'][0]
    var vecData = decodeTradeLogVec(logVec, res['pages'])
    console.log('Trade Log Vec:')
    console.log(vecData)

    //console.log(res)
    //var pageTableHeaderLen = 130
    //var pageSize = 16384
}

async function main() {
    var ndjs
    var market = process.argv[2]
    try {
        ndjs = await fs.readFile(market)
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(ndjs.toString())
    const tradeLogPK = new PublicKey(mktData.tradeLog)
    const tradeLog = await provider.connection.getAccountInfo(tradeLogPK)
    //console.log(orderBook.data)
    decodeTradeLog(tradeLog.data)
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
