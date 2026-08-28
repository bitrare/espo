#!/usr/bin/env python3
"""Compare Fairmints rc3 (8080) vs plugin (8082) RPCs the backend actually uses."""
import json
import sys
import urllib.request

LIVE = "http://127.0.0.1:8080/rpc"
PLUGIN = "http://127.0.0.1:8082/rpc"
HEIGHT = 920000
failed = 0


def rpc(url, method, params=None, timeout=90):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        data = json.load(r)
    err = data.get("error")
    if err:
        raise RuntimeError("%s @ %s: %s" % (method, url, err))
    return data["result"]


def report(name, ok, detail=""):
    global failed
    if ok:
        print("OK   %s" % name)
    else:
        failed += 1
        print("DIFF %s %s" % (name, detail))


def pick(d, keys):
    if not isinstance(d, dict):
        return d
    return {k: d.get(k) for k in keys}


print("=== Fairmints-specific ===")

# marketplaces / tx types
lm = rpc(LIVE, "essentials.get_known_marketplaces")
pm = rpc(PLUGIN, "fairmints.get_known_marketplaces")
report("get_known_marketplaces", lm.get("marketplaces") == pm.get("marketplaces"))

lt = rpc(LIVE, "essentials.get_tx_types")
pt = rpc(PLUGIN, "fairmints.get_tx_types")
report("get_tx_types", lt.get("tx_types") == pt.get("tx_types"))

# block summary + nested diesel
for h in (880000, 900000, 920000, 940000, 960000, 964370):
    ls = rpc(LIVE, "essentials.get_block_summary", {"height": h})
    ps = rpc(PLUGIN, "fairmints.get_block_summary", {"height": h})
    keys = [
        "found", "height", "blockhash", "tx_count", "header_hex",
        "fee_median", "trace_count", "interaction_count",
    ]
    same_core = pick(ls, keys) == pick(ps, keys)
    same_diesel = ls.get("diesel") == ps.get("diesel")
    same_pool = ls.get("pool") == ps.get("pool")
    report(
        "get_block_summary %s" % h,
        same_core and same_diesel and same_pool,
        "core=%s diesel=%s pool=%s live_found=%s plugin_found=%s live_diesel=%s plugin_diesel=%s"
        % (same_core, same_diesel, same_pool, ls.get("found"), ps.get("found"), ls.get("diesel"), ps.get("diesel")),
    )

# historical candles (skip today's bucket)
lc = rpc(LIVE, "ammdata.get_diesel_mint_cost_candles", {"timeframe": "1d", "limit": 8, "page": 20})
pc = rpc(PLUGIN, "fairmints.get_diesel_mint_cost_candles", {"timeframe": "1d", "limit": 8, "page": 20})
report("diesel candles 1d page20", lc.get("candles") == pc.get("candles"))

lc10 = rpc(LIVE, "ammdata.get_diesel_mint_cost_candles", {"timeframe": "10m", "limit": 5, "page": 8})
pc10 = rpc(PLUGIN, "fairmints.get_diesel_mint_cost_candles", {"timeframe": "10m", "limit": 5, "page": 8})
report("diesel candles 10m page8", lc10.get("candles") == pc10.get("candles"))

# btc usd
lp = rpc(LIVE, "ammdata.get_btc_usd_price", {"height": HEIGHT})
pp = rpc(PLUGIN, "fairmints.get_btc_usd_price", {"height": HEIGHT})
report(
    "get_btc_usd_price %s" % HEIGHT,
    lp.get("price") == pp.get("price") or lp.get("price_usd") == pp.get("price_usd") or lp == pp,
    "live=%s plugin=%s" % (json.dumps(lp)[:180], json.dumps(pp)[:180]),
)

# block txs
ltx = rpc(LIVE, "essentials.get_alkane_block_txs", {"height": HEIGHT, "page": 1, "limit": 20, "hide_diesel_mints": True})
ptx = rpc(PLUGIN, "fairmints.get_alkane_block_txs", {"height": HEIGHT, "page": 1, "limit": 20, "hide_diesel_mints": True})
report(
    "get_alkane_block_txs hide_diesel",
    ltx.get("txids") == ptx.get("txids") and ltx.get("total") == ptx.get("total"),
    "live_total=%s plugin_total=%s" % (ltx.get("total"), ptx.get("total")),
)

ltxd = rpc(LIVE, "essentials.get_alkane_block_txs", {"height": HEIGHT, "page": 1, "limit": 10, "tx_type": "diesel_mint"})
ptxd = rpc(PLUGIN, "fairmints.get_alkane_block_txs", {"height": HEIGHT, "page": 1, "limit": 10, "tx_type": "diesel_mint"})
report(
    "get_alkane_block_txs diesel_mint",
    ltxd.get("txids") == ptxd.get("txids") and ltxd.get("total") == ptxd.get("total"),
    "live_total=%s plugin_total=%s" % (ltxd.get("total"), ptxd.get("total")),
)

# full txs
lf = rpc(LIVE, "essentials.get_alkane_block_txs_full", {"height": HEIGHT, "page": 1, "limit": 5})
pf = rpc(PLUGIN, "fairmints.get_alkane_block_txs_full", {"height": HEIGHT, "page": 1, "limit": 5})
lrows = lf.get("transactions") or []
prows = pf.get("transactions") or []
report(
    "get_alkane_block_txs_full totals",
    lf.get("total") == pf.get("total") and len(lrows) == len(prows),
    "live_total=%s plugin_total=%s live_n=%s plugin_n=%s" % (lf.get("total"), pf.get("total"), len(lrows), len(prows)),
)

type_ok = True
bc_ok = True
txid_for_summary = None
for a, b in zip(lrows, prows):
    if a.get("txid") != b.get("txid") or a.get("tx_type") != b.get("tx_type"):
        type_ok = False
    if bool(a.get("balance_changes")) != bool(b.get("balance_changes")):
        bc_ok = False
    if txid_for_summary is None:
        txid_for_summary = a.get("txid")
report("get_alkane_block_txs_full txid+type", type_ok)
report("get_alkane_block_txs_full has balance_changes", bc_ok)

if txid_for_summary:
    ls = rpc(LIVE, "essentials.get_alkane_tx_summary", {"txid": txid_for_summary})
    ps = rpc(PLUGIN, "fairmints.get_alkane_tx_summary", {"txid": txid_for_summary})
    report(
        "get_alkane_tx_summary type+mp",
        ls.get("tx_type") == ps.get("tx_type") and ls.get("marketplace_info") == ps.get("marketplace_info"),
        "live_type=%s plugin_type=%s" % (ls.get("tx_type"), ps.get("tx_type")),
    )
    report(
        "get_alkane_tx_summary balance_changes present",
        ("balance_changes" in ps) and (bool(ls.get("balance_changes")) == bool(ps.get("balance_changes"))),
        "live_bc=%s plugin_bc=%s" % (bool(ls.get("balance_changes")), bool(ps.get("balance_changes"))),
    )

# outpoints from first full tx if any
if lrows:
    bc = lrows[0].get("balance_changes") or {}
    outs = []
    for side in ("inputs", "outputs"):
        for e in bc.get(side) or []:
            if e.get("outpoint"):
                outs.append(e["outpoint"])
    outs = outs[:4]
    if outs:
        lo = rpc(LIVE, "essentials.check_outpoints_spent", {"outpoints": outs})
        po = rpc(PLUGIN, "fairmints.check_outpoints_spent", {"outpoints": outs})
        report("check_outpoints_spent", lo.get("results") == po.get("results"), "live=%s plugin=%s" % (lo, po))

# mempool: shape only
lm = rpc(LIVE, "essentials.get_mempool_alkane_txs_full", {"page": 1, "limit": 5, "hide_diesel_mints": True})
pm = rpc(PLUGIN, "fairmints.get_mempool_alkane_txs_full", {"page": 1, "limit": 5, "hide_diesel_mints": True})
report(
    "get_mempool_alkane_txs_full shape",
    set(lm.keys()) >= {"ok", "transactions"} and set(pm.keys()) >= {"ok", "transactions"},
    "live_keys=%s plugin_keys=%s" % (sorted(lm.keys()), sorted(pm.keys())),
)

print("\n=== Stock methods (should stay on essentials/ammdata) ===")
try:
    lh = rpc(LIVE, "get_espo_height")
    ph = rpc(PLUGIN, "get_espo_height")
    report(
        "get_espo_height close",
        abs(int(lh.get("height", 0)) - int(ph.get("height", 0))) <= 6,
        "live=%s plugin=%s" % (lh, ph),
    )
except Exception as e:
    report("get_espo_height", False, str(e))

try:
    la = rpc(LIVE, "essentials.get_alkane_info", {"alkane": "2:0"})
    pa = rpc(PLUGIN, "essentials.get_alkane_info", {"alkane": "2:0"})
    report(
        "essentials.get_alkane_info 2:0 name",
        la.get("name") == pa.get("name") or la.get("symbol") == pa.get("symbol"),
        "live=%s plugin=%s" % (json.dumps(la)[:160], json.dumps(pa)[:160]),
    )
except Exception as e:
    report("essentials.get_alkane_info 2:0", False, str(e))

try:
    lp = rpc(LIVE, "ammdata.get_pools", {"limit": 3, "page": 1})
    pp = rpc(PLUGIN, "ammdata.get_pools", {"limit": 3, "page": 1})
    report(
        "ammdata.get_pools responds",
        bool(lp) and bool(pp),
        "live_keys=%s plugin_keys=%s" % (
            sorted(lp.keys()) if isinstance(lp, dict) else type(lp).__name__,
            sorted(pp.keys()) if isinstance(pp, dict) else type(pp).__name__,
        ),
    )
except Exception as e:
    report("ammdata.get_pools", False, str(e))

print("\n%s failures" % failed)
sys.exit(1 if failed else 0)
