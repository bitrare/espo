#!/usr/bin/env python3
"""Certify a RAREPEPE snapshot only after independent legacy and Core audits."""
import json, urllib.request, urllib.parse, time, subprocess, sys, collections, hashlib
from pathlib import Path
out=Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=True)
core_url='http://127.0.0.1:4100/v2'
def fetch(url,body=None):
    req=urllib.request.Request(url,data=json.dumps(body).encode() if body else None,headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req,timeout=90) as response:
        value=json.load(response);headers=dict(response.headers)
    if value.get('error'):raise RuntimeError(value['error'])
    return value,headers
def query(method,params):
    v,_=fetch('http://127.0.0.1:9093/query',{'method':method,'params':params})
    assert v.get('ok'),v
    return v
def core(path):
    v,h=fetch(core_url+path);assert 'result' in v,v
    return v,h
def core_rows(path):
    rows=[];cursor=None;heights=set()
    while True:
        suffix=('&' if '?' in path else '?')+'limit=1000'+('&cursor='+urllib.parse.quote(str(cursor)) if cursor is not None else '')
        v,h=core(path+suffix); rows.extend(v['result'])
        heights.add(int(next(val for key,val in h.items() if key.lower()=='x-counterparty-height')))
        new=v.get('next_cursor')
        if new is None:break
        assert new!=cursor,'non-advancing Core cursor'
        cursor=new;time.sleep(.5)
    assert len(heights)==1,'Core height changed during pagination'
    return rows,heights.pop()
def loc_key(loc):return loc['kind']+':'+loc['id']
def expand(r):
    a,b=int(r['first']),int(r['last']);step=1 if b>=a else -1
    units=list(range(a,b+step,step));assert len(units)==int(r['quantity'])
    return units
params={'asset':'RAREPEPE','limit':200};snapshot=[];generations=set()
while True:
    page=query('get_asset_units',params);snapshot.extend(page['items']);generations.add(page['context']['generation'])
    if not page['has_more']:break
    params['cursor']=page['next_cursor']
assert len(generations)==1 and page['context']['complete'],'snapshot changed or incomplete'
context=page['context'];checkpoint=context['numbering_tip'];height=checkpoint['height']
(out/'asset-units.json').write_text(json.dumps({'context':context,'items':snapshot},indent=2))
subprocess.run([sys.executable,str(Path(__file__).with_name('audit_rarepepe.py')),str(out/'histories'),str(height)],check=True)
history_report=json.loads((out/'histories/history-report.json').read_text())
assert history_report['histories_matched']==300 and history_report['historical_cut']==725934,history_report
moves=[m for m in json.loads((out/'histories/movements.json').read_text()) if m['block']<=height]
stacks={};locations={};legacy=None
for m in moves:
    if legacy is None and m['block']>725934:legacy={k:v.copy() for k,v in stacks.items() if v}
    units=[s for r in m['ranges'] for s in expand(r)]
    if m['from']:
        source=stacks[loc_key(m['from'])]
        assert set(units)<=set(source),'history spends missing units'
        for serial in units:source.remove(serial)
    else:assert not (set(units)&{s for v in stacks.values() for s in v}),'serial issued twice'
    key=loc_key(m['to']);stacks.setdefault(key,[]).extend(units);locations[key]=m['to']
    assert len({s for v in stacks.values() for s in v})==sum(map(len,stacks.values())),'duplicate ownership'
if legacy is None:legacy={k:v.copy() for k,v in stacks.items() if v}
gold=json.loads(Path('/var/www/espo-xcp/tests/fixtures/xcp_numbering/legacy_RAREPEPE.json').read_text())
assert legacy==gold['holdings'],'legacy ordered stacks differ'
expected=collections.defaultdict(list)
for row in snapshot:expected[loc_key(row['location'])].append(row)
actual={}
for key,rows in expected.items():
    rows.sort(key=lambda r:int(r['stack_position']));ordered=[]
    for row in rows:
        assert int(row['stack_position'])==len(ordered),'stack position gap'
        ordered.extend(expand(row['range']))
    actual[key]=ordered
assert actual=={k:v for k,v in stacks.items() if v},'current history differs from current ordered holdings'
serials=[s for v in actual.values() for s in v]
assert sorted(serials)==list(range(1,301)), 'missing or duplicate serial'
# Read Core at one stable height. The six-confirmation buffer may differ from
# that height only if the full intervening event window has no RAREPEPE effect.
core_before=core('/')[0]['result'];core_height=core_before['counterparty_height'];assert core_before['server_ready']
assert core_before['network']==context['network'] and core_before['current_commit']==context['source_commit'] and core_before['version']==context['core_version']
asset_info,info_headers=core('/assets/RAREPEPE')
assert int(next(v for k,v in info_headers.items() if k.lower()=='x-counterparty-height'))==core_height
balances,balance_height=core_rows('/assets/RAREPEPE/balances')
holders,holder_height=core_rows('/assets/RAREPEPE/holders')
assert balance_height==holder_height==core_height,'Core reads span different heights'
core_wallets={('utxo:'+r['utxo']) if r.get('utxo') else ('address:'+r['address']):r['quantity'] for r in balances if r['quantity']}
numbered_wallets={k:len(v) for k,v in actual.items() if locations[k]['kind'] in ['address','utxo']}
assert core_wallets==numbered_wallets, {'wallet_difference':{k:(numbered_wallets.get(k),core_wallets.get(k)) for k in set(core_wallets)|set(numbered_wallets) if numbered_wallets.get(k)!=core_wallets.get(k)}}
escrow_types={'open_dispenser':'dispenser','open_order':'order','pending_order_match':'pending_btc_match'}
core_escrows=collections.Counter()
for row in holders:
    if not row['quantity'] or row['holding_type']=='balances':continue
    assert row['holding_type'] in escrow_types, row
    core_escrows[escrow_types[row['holding_type']]+':'+row['escrow']]+=row['quantity']
numbered_escrows={k:len(v) for k,v in actual.items() if locations[k]['kind'] not in ['address','utxo','destroyed','retired']}
assert dict(core_escrows)==numbered_escrows, {'numbered_escrows':numbered_escrows,'core_escrows':dict(core_escrows)}
destroyed=sorted(s for k,v in actual.items() if locations[k]['kind']=='destroyed' for s in v)
retired=sorted(s for k,v in actual.items() if locations[k]['kind']=='retired' for s in v)
surviving=300-len(destroyed)-len(retired)
assert asset_info['result']['supply']==surviving and not asset_info['result']['divisible']
anchor_rows,_=core_rows(f'/blocks/{height}/events');anchor_rows.sort(key=lambda e:e['event_index'])
assert anchor_rows[0]['params']['block_hash']==checkpoint['hash'] and anchor_rows[-1]['params']['messages_hash']==checkpoint['messages_hash'],'numbering anchor changed'
assert anchor_rows[0]['event_index']==checkpoint['first'] and anchor_rows[-1]['event_index']==checkpoint['last']
window=[]; previous_hash=checkpoint['hash']; previous_event=checkpoint['last']
for h in range(height+1,core_height+1):
    events,_=core_rows(f'/blocks/{h}/events');events.sort(key=lambda e:e['event_index'])
    assert events[0]['event']=='NEW_BLOCK' and events[-1]['event']=='BLOCK_PARSED'
    assert events[0]['params']['previous_block_hash']==previous_hash and events[0]['event_index']==previous_event+1
    previous_hash=events[0]['params']['block_hash'];previous_event=events[-1]['event_index']
    assert all(b['event_index']==a['event_index']+1 for a,b in zip(events,events[1:]))
    for e in events:
        assert not any(e['params'].get(f)=='RAREPEPE' for f in ['asset','asset_a','asset_b','forward_asset','backward_asset','give_asset','get_asset']),{'RAREPEPE_changed_above_checkpoint':h,'event':e}
    window.append({'height':h,'block_hash':events[0]['params']['block_hash'],'messages_hash':events[-1]['params']['messages_hash'],'event_count':len(events)})
    time.sleep(.5)
core_after=core('/')[0]['result'];assert core_after['counterparty_height']==core_height and core_after['server_ready'],'Core advanced during audit'
evidence={'checkpoint':checkpoint,'core_height':core_height,'core_asset':asset_info,'core_balances':balances,'core_holders':holders,'confirmation_window':window,'destroyed_serials':destroyed,'retired_serials':retired,'historical_report':{k:v for k,v in history_report.items() if k!='contexts'}}
evidence_bytes=json.dumps(evidence,sort_keys=True,indent=2).encode();(out/'audit-evidence.json').write_bytes(evidence_bytes)
certificate={'schema':1,'asset':'RAREPEPE','network':context['network'],'ruleset':context['ruleset'],'source_commit':context['source_commit'],'checkpoint':checkpoint,'last_movement':{k:moves[-1][k] for k in ['event_index','leg']},'issued_units':300,'surviving_units':surviving,'destroyed_serials':destroyed,'retired_serials':retired,'core_height':core_height,'historical_cut':725934,'histories_matched':300,'wallet_locations':len(core_wallets),'escrow_locations':len(core_escrows),'checked_at':int(time.time()),'checks':{k:True for k in ['all_serial_histories','legacy_ordered_holdings','current_ordered_holdings','unique_serials','core_balances','core_escrows','core_supply','core_height_window']},'evidence_sha256':hashlib.sha256(evidence_bytes).hexdigest()}
(out/'certificate.json').write_text(json.dumps(certificate,indent=2))
print(json.dumps(certificate,indent=2),flush=True)
