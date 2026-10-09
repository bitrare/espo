#!/usr/bin/env python3
"""Read-only RAREPEPE history comparison through a covered legacy checkpoint."""
import json, time, urllib.request, hashlib, sys
from pathlib import Path
out = Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)
gold_path = Path('/var/www/espo-xcp/tests/fixtures/xcp_numbering/legacy_RAREPEPE.json')
gold = json.loads(gold_path.read_text())
required_height=int(sys.argv[2]) if len(sys.argv)>2 else 0
def request(method, params):
    body = json.dumps({'method':method, 'params':params}).encode()
    req = urllib.request.Request('http://127.0.0.1:9093/query', data=body, headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req, timeout=90) as res: data=json.load(res)
    if not data.get('ok'): raise RuntimeError(data)
    return data
start=request('get_numbering_status',{})
cut=min(start['numbering_tip']['height'], max(op['block'] for op in gold['operations']))
union={}; differences=[]; contexts={}; checked=0
for serial in range(1,301):
    cache=out/f'unit-{serial}.json'
    record=json.loads(cache.read_text()) if cache.exists() else None
    if record is None or record['context']['numbering_tip']['height'] < required_height:
        for attempt in range(5):
            params={'asset':'RAREPEPE','serial':str(serial),'allow_partial':True,'limit':200}; rows=[]; generations=set()
            try:
                while True:
                    page=request('get_unit_history',params)
                    generations.add(page['context']['generation'])
                    rows.extend(page['items'])
                    if not page['has_more']: break
                    if not page['next_cursor'] or page['next_cursor']==params.get('cursor'): raise RuntimeError('non-advancing cursor')
                    params['cursor']=page['next_cursor']
                if len(generations)!=1: raise RuntimeError('changed generation')
                record={'context':page['context'],'items':rows};cache.write_text(json.dumps(record));break
            except RuntimeError as exc:
                if 'stale_cursor' not in str(exc) and 'changed generation' not in str(exc): raise
                if attempt==4: raise
                time.sleep(1)
        time.sleep(.08)
    contexts[record['context']['generation']]=record['context']['numbering_tip']
    assert record['context']['numbering_tip']['height']>=cut
    observed=[]
    for row in record['items']:
        union[(int(row['event_index']),row['leg'])]=row
        if row['block']>cut: continue
        loc=row['to']; entry={'address':loc.get('controller') or loc['id'],'fromBlock':row['block']}
        if loc['kind']=='order':entry['orderId']=loc['id']
        elif loc['kind']=='dispenser':entry['dispenser']=loc['id']
        elif loc['kind']!='address':entry['location_kind']=loc['kind']
        observed.append(entry)
    expected=[row for row in gold['histories'][f'#{serial}'] if row['fromBlock']<=cut]
    if observed!=expected:
        first=next((i for i,(a,b) in enumerate(zip(observed,expected)) if a!=b),min(len(observed),len(expected)))
        differences.append({'serial':serial,'observed_count':len(observed),'expected_count':len(expected),'first_difference':first,'observed':observed[max(0,first-1):first+3],'expected':expected[max(0,first-1):first+3]})
    checked+=1
    if serial%50==0: print(json.dumps({'checked':checked,'mismatches':len(differences),'cut':cut}),flush=True)
rows=[union[k] for k in sorted(union)]
(out/'movements.json').write_text(json.dumps(rows))
report={'asset':'RAREPEPE','historical_cut':cut,'legacy_full_cut':max(op['block'] for op in gold['operations']),'serials_checked':checked,'histories_matched':300-len(differences),'differences':differences,'unique_movements':len(rows),'contexts':contexts,'legacy_fixture_sha256':hashlib.sha256(gold_path.read_bytes()).hexdigest()}
(out/'history-report.json').write_text(json.dumps(report,indent=2))
print(json.dumps({k:v for k,v in report.items() if k not in ['differences','contexts']},indent=2),flush=True)
