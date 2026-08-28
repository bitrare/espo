#!/usr/bin/env node
/**
 * Compare Fairmints-specific RPCs on two Espo nodes.
 *
 *   RC3=http://127.0.0.1:8080/rpc \
 *   PLUGIN=http://127.0.0.1:8082/rpc \
 *   HEIGHT=920000 \
 *   TXID=<alkane txid> \
 *   node scripts/parity-fairmints.mjs
 *
 * Plugin methods are called as fairmints.*; rc3 methods use the old names.
 */

const rc3 = process.env.RC3 || 'http://127.0.0.1:8080/rpc';
const plugin = process.env.PLUGIN || 'http://127.0.0.1:8082/rpc';
const height = Number(process.env.HEIGHT || 920000);
const txid = process.env.TXID || '';

async function rpc(url, method, params = {}) {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
  return body.result;
}

function cmp(name, a, b, ignore = []) {
  const strip = (v) => {
    if (v && typeof v === 'object' && !Array.isArray(v)) {
      const out = { ...v };
      for (const k of ignore) delete out[k];
      return out;
    }
    return v;
  };
  const sa = JSON.stringify(strip(a));
  const sb = JSON.stringify(strip(b));
  if (sa === sb) {
    console.log(`OK   ${name}`);
    return true;
  }
  console.log(`DIFF ${name}`);
  console.log('  rc3   ', sa.slice(0, 400));
  console.log('  plugin', sb.slice(0, 400));
  return false;
}

const cases = [
  ['get_known_marketplaces', 'essentials.get_known_marketplaces', 'fairmints.get_known_marketplaces', {}],
  ['get_tx_types', 'essentials.get_tx_types', 'fairmints.get_tx_types', {}],
  [
    'get_alkane_block_txs_full',
    'essentials.get_alkane_block_txs_full',
    'fairmints.get_alkane_block_txs_full',
    { height, page: 1, limit: 20 },
  ],
  [
    'get_alkane_block_txs hide_diesel',
    'essentials.get_alkane_block_txs',
    'fairmints.get_alkane_block_txs',
    { height, page: 1, limit: 20, hide_diesel_mints: true },
  ],
  [
    'get_block_summary',
    'essentials.get_block_summary',
    'fairmints.get_block_summary',
    { height },
  ],
  [
    'get_diesel_mint_cost_candles',
    'ammdata.get_diesel_mint_cost_candles',
    'fairmints.get_diesel_mint_cost_candles',
    { timeframe: '1d', limit: 10, page: 1 },
  ],
  [
    'get_btc_usd_price',
    'ammdata.get_btc_usd_price',
    'fairmints.get_btc_usd_price',
    { height },
  ],
  [
    'get_mempool_alkane_txs_full',
    'essentials.get_mempool_alkane_txs_full',
    'fairmints.get_mempool_alkane_txs_full',
    { page: 1, limit: 10, hide_diesel_mints: true },
  ],
  [
    'get_mempool next_block',
    'essentials.get_mempool_alkane_txs_full',
    'fairmints.get_mempool_alkane_txs_full',
    { page: 1, limit: 10, next_block_only: true },
  ],
];

if (txid) {
  cases.push([
    'get_alkane_tx_summary',
    'essentials.get_alkane_tx_summary',
    'fairmints.get_alkane_tx_summary',
    { txid },
  ]);
}

let failed = 0;
for (const [name, left, right, params] of cases) {
  try {
    const a = await rpc(rc3, left, params);
    const b = await rpc(plugin, right, params);
    if (!cmp(name, a, b, ['ok'])) failed += 1;
  } catch (e) {
    console.log(`ERR  ${name}: ${e.message}`);
    failed += 1;
  }
}

process.exit(failed ? 1 : 0);
