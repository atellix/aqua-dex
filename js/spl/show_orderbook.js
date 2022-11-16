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

function decodeOrderNode(tag, blob) {
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
        const stPrice = lo.struct([
            lo.blob(8),
            lo.nu64('price'),
        ])
        data = stLeafNode.decode(blob)
        //data['price'] = bigintConv.bufToBigint(data['key']) >> BigInt(64)
        data['price'] = stPrice.decode(data['key'])['price']
        data['key'] = encodeOrderId(data['key'])
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

function decodeOrdersMap(pageTableEntry, pages) {
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
            nodeSpec['nodes'].push(decodeOrderNode(nodeTag['tag'], nodeBlob))
            if (nodeSpec['nodes'].length === pageTableEntry['alloc_items']) {
                i = mapPages.length
                break
            }
        }
    }
    return nodeSpec
}

function decodeOrdersVec(pageTableEntry, pages) {
    const headerSize = pageTableEntry['header_size']
    const offsetSize = pageTableEntry['offset_size']
    const stOrder = lo.struct([
        lo.nu64('amount'),
        lo.ns64('expiry'),
    ])
    const instPerPage = Math.floor((16384 - (headerSize + offsetSize)) / stOrder.span)
    const stSlabVec = lo.struct([
        lo.blob(offsetSize),
        lo.u32('free_top'),
        lo.u32('next_index'),
        lo.seq(stOrder, instPerPage, 'orders'),
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
    var orderSpec = {
        'orders': [],
    }
    for (var i = 0; i < vecPages.length; i++) {
        var res = stSlabVec.decode(vecPages[i])
        if (i === 0) {
            orderSpec['free_top'] = res['free_top']
            orderSpec['next_index'] = res['next_index']
        }
        for (var orderIdx = 0; orderIdx < res['orders'].length; orderIdx++) {
            orderSpec['orders'].push(res['orders'][orderIdx])
            if (orderSpec['orders'].length === pageTableEntry['alloc_items']) {
                i = vecPages.length
                break
            }
        }
    }
    return orderSpec
}

function decodeOrderBookSide(side, mapData, vecData) {
    var orderBook = []
    for (var i = 0; i < mapData['nodes'].length; i++) {
        var node = mapData['nodes'][i]
        if (node && node.tag === 2) {
            var order = vecData['orders'][node.slot]
            var orderItem = {
                'type': side,
                'key': node['key'],
                'price': node['price'],
                'owner': node['owner'],
                'amount': order['amount'],
                'expiry': order['expiry'],
            }
            orderBook.push(orderItem)
        }
    }
    return orderBook
}

function decodeOrderBook(data) {
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
    var bidMap = res['type_page'][0]
    var askMap = res['type_page'][1]
    var bidVec = res['type_page'][2]
    var askVec = res['type_page'][3]

    var bidVecData = decodeOrdersVec(bidVec, res['pages'])
    var askVecData = decodeOrdersVec(askVec, res['pages'])
    var bidMapData = decodeOrdersMap(bidMap, res['pages'])
    var askMapData = decodeOrdersMap(askMap, res['pages'])
    console.log(decodeOrderBookSide('bid', bidMapData, bidVecData))
    console.log(decodeOrderBookSide('ask', askMapData, askVecData))
    
    /*console.log('Bid Vec:')
    console.log(bidVecData)
    console.log('Ask Vec:')
    console.log(askVecData)
    console.log('Bid Map:')
    console.log(bidMapData)
    console.log('Ask Map:')
    console.log(askMapData)*/

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
    const orderBookPK = new PublicKey(mktData.orders)
    const orderBook = await provider.connection.getAccountInfo(orderBookPK)
    //console.log(orderBook.data)
    decodeOrderBook(orderBook.data)
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
