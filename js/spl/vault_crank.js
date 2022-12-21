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

async function programAddress(inputs, program = aquadexPK) {
    const addr = await PublicKey.findProgramAddress(inputs, program)
    const res = { 'pubkey': await addr[0].toString(), 'nonce': addr[1] }
    return res
}

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

function decodeSettlementNode(tag, blob) {
    var data
    if (tag === 0) {
        data = null
    } else if (tag === 1) {
        const stInnerNode = lo.struct([
            lo.u32('tag'),
            lo.blob(16, 'key'),
            lo.u32('prefix_len'),
            lo.seq(lo.u32(), 2, 'children'),
            lo.blob(24),
        ])
        data = stInnerNode.decode(blob)
    } else if (tag === 2) {
        const stLeafNode = lo.struct([
            lo.u32('tag'),
            lo.u32('slot'),
            lo.blob(16, 'key'),
            lo.blob(32, 'owner'),
        ])
        data = stLeafNode.decode(blob)
        data['owner'] = (new PublicKey(data['owner'].toJSON().data)).toString()
    } else if (tag === 3 || tag === 4) {
        const stFreeNode = lo.struct([
            lo.u32('tag'),
            lo.u32('next'),
            lo.blob(48),
        ])
        data = stFreeNode.decode(blob)
    }
    return data
}

function decodeSettlementMap(pageTableEntry, pages) {
    const headerSize = pageTableEntry['header_size']
    const offsetSize = pageTableEntry['offset_size']
    const stNode = lo.struct([
        lo.u32('tag'),
        lo.blob(52)
    ])
    const instPerPage = Math.floor((16384 - (headerSize + offsetSize)) / stNode.span)
    const stSlabVec = lo.struct([
        lo.blob(offsetSize),
        lo.nu64('bump_index'),
        lo.nu64('free_list_len'),
        lo.u32('free_list_head'),
        lo.u32('root_node'),
        lo.nu64('leaf_count'),
        lo.seq(lo.blob(stNode.span), instPerPage, 'nodes'),
    ])
    var totalPages = Math.floor(pageTableEntry['alloc_items'] / instPerPage)
    if ((pageTableEntry['alloc_items'] % instPerPage) !== 0) {
        totalPages = totalPages + 1
    }
    var mapPages = []
    for (var p = 0; p < totalPages; p++) {
        var pidx = pageTableEntry['alloc_pages'][p]
        mapPages.push(pages[pidx])
    }
    var nodeSpec = {
        'nodes': [],
    }
    for (var i = 0; i < mapPages.length; i++) {
        var res = stSlabVec.decode(mapPages[i])
        if (i === 0) {
            nodeSpec['bump_index'] = res['bump_index']
            nodeSpec['free_list_len'] = res['free_list_len']
            nodeSpec['free_list_head'] = res['free_list_head']
            nodeSpec['root_node'] = res['root_node']
            nodeSpec['leaf_count'] = res['leaf_count']
        }
        for (var nodeIdx = 0; nodeIdx < res['nodes'].length; nodeIdx++) {
            var nodeBlob = res['nodes'][nodeIdx]
            var nodeTag = stNode.decode(nodeBlob)
            nodeSpec['nodes'].push(decodeSettlementNode(nodeTag['tag'], nodeBlob))
            if (nodeSpec['nodes'].length === pageTableEntry['alloc_items']) {
                i = mapPages.length
                break
            }
        }
    }
    return nodeSpec
}

function decodeSettlementVec(pageTableEntry, pages) {
    const headerSize = pageTableEntry['header_size']
    const offsetSize = pageTableEntry['offset_size']
    const stEntry = lo.struct([
        lo.nu64('mkt_token_balance'),
        lo.nu64('prc_token_balance'),
        lo.ns64('ts_updated'),
    ])
    const instPerPage = Math.floor((16384 - (headerSize + offsetSize)) / stEntry.span)
    const stSlabVec = lo.struct([
        lo.blob(offsetSize),
        lo.u32('free_top'),
        lo.u32('next_index'),
        lo.seq(stEntry, instPerPage, 'entries'),
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
    var entrySpec = {
        'entries': [],
    }
    for (var i = 0; i < vecPages.length; i++) {
        var res = stSlabVec.decode(vecPages[i])
        if (i === 0) {
            entrySpec['free_top'] = res['free_top']
            entrySpec['next_index'] = res['next_index']
        }
        for (var entryIdx = 0; entryIdx < res['entries'].length; entryIdx++) {
            entrySpec['entries'].push(res['entries'][entryIdx])
            if (entrySpec['entries'].length === pageTableEntry['alloc_items']) {
                i = vecPages.length
                break
            }
        }
    }
    return entrySpec
}

function decodeSettlementLog(data) {
    const stAccountsHeader = lo.struct([
        lo.blob(32, 'market'),
        lo.blob(32, 'prev'),
        lo.blob(32, 'next'),
        lo.u32('items'),
    ]);
    const stTypedPage = lo.struct([
        lo.nu64('header_size'),
        lo.nu64('offset_size'),
        lo.nu64('alloc_items'),
        lo.seq(lo.u16('page_index'), 16, 'alloc_pages'), // TYPE_MAX_PAGES
    ]);
    const stSlabAlloc = lo.struct([
        lo.seq(stAccountsHeader, 1, 'header'),
        lo.u16('top_unused_page'),
        lo.seq(stTypedPage, 4, 'type_page'), // TYPE_MAX
        lo.seq(lo.blob(16384), 6, 'pages'), // PAGE_SIZE
    ]);
    var res = stSlabAlloc.decode(data)
    var header = res['header']
    var marketPK = header[0]['market'].toJSON().data
    var prevPK = new PublicKey(header[0]['prev'].toJSON().data)
    var nextPK = new PublicKey(header[0]['next'].toJSON().data)
    
    /*console.log({
        'market': (new PublicKey(marketPK)).toString(),
        'prev': (new PublicKey(prevPK)).toString(),
        'next': (new PublicKey(nextPK)).toString(),
        'items': header[0]['items'],
    })*/
    //console.log(JSON.stringify(res['header'], null, 4))
    //console.log(JSON.stringify(res['type_page']))
    var settleMap = res['type_page'][0]
    var settleVec = res['type_page'][1]

    var mapData = decodeSettlementMap(settleMap, res['pages'])
    var vecData = decodeSettlementVec(settleVec, res['pages'])
    var settlementEntries = []
    for (var i = 0; i < mapData['nodes'].length; i++) {
        var node = mapData['nodes'][i]
        if (node && node.tag === 2) {
            var entry = vecData['entries'][node.slot]
            var entryItem = {
                'owner': node['owner'],
                'mkt_token_balance': entry['mkt_token_balance'],
                'prc_token_balance': entry['prc_token_balance'],
                'ts_updated': entry['ts_updated'],
            }
            settlementEntries.push(entryItem)
        }
    }
    return {
        'prev': prevPK,
        'next': nextPK,
        'entries': settlementEntries,
    }
    //console.log(settlementEntries)

    /*console.log('Settlement Vec:')
    console.log(vecData)
    console.log('Settlement Map:')
    console.log(mapData)*/
}

async function main() {
    var market = process.argv[2]
    const marketPK = new PublicKey(market)
    const marketData = await aquadex.account.market.fetch(marketPK)
    const marketStatePK = marketData.state
    const marketSpec = await aquadex.account.marketState.fetch(marketStatePK)
    const settlePK = marketSpec.settleA
    const settle = await provider.connection.getAccountInfo(settlePK)
    //console.log(settle0.data)
    const logs = decodeSettlementLog(settle.data)
    const wallet = provider.wallet.publicKey.toString()
    const now = new Date()
    for (const l of logs.entries) {
        const logDate = new Date(l.ts_updated * 1000)
        const logDiff = Math.floor(Math.abs(logDate.getTime() - now.getTime()) / 1000) // Seconds
        //console.log(logDate + ' ' + logDiff)
        // Find entries not updated within the past X minute(s)
        if (logDiff >= (1 * 60)) {
            const vaultOwnerPK = new PublicKey(l.owner)
            const vault = await programAddress([marketPK.toBuffer(), vaultOwnerPK.toBuffer()], aquadexPK)
            const admin = await programAddress([marketPK.toBuffer(), Buffer.from('admin', 'utf8')], aquadexPK)
            var prevPK = logs.prev
            var nextPK = logs.next
            if (prevPK.toString() === '11111111111111111111111111111111') {
                prevPK = settlePK
            }
            if (nextPK.toString() === '11111111111111111111111111111111') {
                nextPK = settlePK
            }
            var res = await aquadex.rpc.vaultDeposit(
                {
                    accounts: {
                        market: marketPK,
                        state: marketStatePK,
                        admin: new PublicKey(admin.pubkey),
                        manager: provider.wallet.publicKey,
                        owner: new PublicKey(l.owner),
                        settle: settlePK,
                        settlePrev: prevPK,
                        settleNext: nextPK,
                        vault: new PublicKey(vault.pubkey),
                        systemProgram: SystemProgram.programId,
                    },
                },
            )
            console.log('Vault Deposit: ' + l.owner + ' ' + res)
            //console.log(res)
        }
    }
}

main().then(() => process.exit(0)).catch(error => {
    console.log(error)
})
