#!/usr/bin/env python3
"""Build offline convention fixtures from the original XCPToNFT archive.
This is not a substitute for replaying the current Core event adapter.
Usage: prepare_legacy_numbering_fixture.py /path/to/XCPToNFT
"""
import json,sys,hashlib
from pathlib import Path
base=Path(sys.argv[1]);out=Path(__file__).resolve().parents[1]/'tests/fixtures/xcp_numbering'
for asset in ['RAREPEPE','DJPEPE']:
    raw_path=base/f'saves/assets/{asset}.json';gold_path=base/f'saves/assets_ids/{asset}.json'
    raw=json.loads(raw_path.read_text());gold=json.loads(gold_path.read_text());stacks={};hist={};ops=[];controllers={};counter=0
    def location(kind,id,controller=None):
        return {'kind':kind,'id':id,'controller':controller or (id if kind=='address' else None),'ledger':f'address:{id}' if kind=='address' else None}
    def addr(a):return location('address',a)
    def move(v,source,dest,q,reverse=True):
        if q==0:return
        a=source['kind']+':'+source['id'];b=dest['kind']+':'+dest['id'];s=stacks.setdefault(a,[])
        assert len(s)>=q,(asset,v['block'],v['event'],v['type'],q,s)
        units=s[-q:];del s[-q:]
        if reverse:units.reverse()
        stacks.setdefault(b,[]).extend(units)
        ops.append({'block':v['block'],'from':source,'to':dest,'quantity':q,'reverse':reverse,'reason':v['event']+'/'+v['type']})
        for serial in units:hist[serial].append((v['block'],dest))
    for v in raw:
        ev=v['event'];typ=v.get('type')
        if ev=='ISSUANCE' and v['issuance']['quantity']>0 and v['issuance']['status']=='valid':
            p=v['issuance'];q=p['quantity'];dest=addr(p['issuer']);units=list(range(counter+1,counter+q+1));counter+=q
            stacks.setdefault('address:'+p['issuer'],[]).extend(units)
            ops.append({'block':v['block'],'from':None,'to':dest,'quantity':q,'reverse':False,'reason':'issuance'})
            for serial in units:hist[serial]=[(v['block'],dest)]
        elif ev=='SENDS' and typ=='CLASSIC' and v.get('status')=='valid':move(v,addr(v['from']),addr(v['to']),v['amount'])
        elif ev=='SENDS' and typ=='MPMA SEND':
            for a,d,q,*_ in v['mpma_sends']:
                if a==asset:move(v,addr(v['from']),addr(d),q)
        elif ev=='SWEEPS':
            for a,q in v['balances']:
                if a==asset:move(v,addr(v['from']),addr(v['to']),q)
        elif ev=='ORDER' and typ=='NEW ORDER' and v['order']['give_asset']==asset:
            controllers[v['tx_hash']]=v['for'];move(v,addr(v['for']),location('order',v['tx_hash'],v['for']),v['debit'])
        elif ev=='ORDER' and typ in ['CANCELED','EXPIRED']:
            move(v,location('order',v['order_id'],v['for_address']),addr(v['for_address']),v['credit'])
        elif ev=='ORDER' and typ=='ORDER MATCH' and v['status']=='completed':
            p=v['data'];side,source,dest=('backward','tx1','tx0') if p['backward_asset']==asset else ('forward','tx0','tx1')
            move(v,location('order',p[source+'_hash'],p[source+'_address']),addr(p[dest+'_address']),p[side+'_quantity'])
        elif ev=='DISPENSER' and typ=='OPEN DISPENSER':
            controllers[v['tx_hash']]=v['at_address'];move(v,addr(v['issuer']),location('dispenser',v['tx_hash'],v['at_address']),v['amount'])
        elif ev=='DISPENSER' and typ=='REFILL':
            owner=v['dispenser']['creator'];id=next(k[10:] for k,s in stacks.items() if k.startswith('dispenser:') and s and controllers[k[10:]]==owner)
            move(v,addr(owner),location('dispenser',id,owner),v['dispenser']['refill_quantity'],False)
        elif ev=='DISPENSER' and typ=='CLOSING':
            owner=v['for_address'];id=next(k[10:] for k,s in stacks.items() if k.startswith('dispenser:') and s and controllers[k[10:]]==owner)
            move(v,location('dispenser',id,owner),addr(owner),v['credit'])
        elif ev=='DISPENSER' and typ=='DISPENSE':
            p=v['dispense'];move(v,location('dispenser',p['dispenser_id'],p['dispenser_address']),addr(v['for']),p['amount'])
    histories={}
    for serial,entries in hist.items():
        history=[]
        for block,loc in entries:
            row={'address':loc['controller'] or loc['id'],'fromBlock':block}
            if loc['kind']=='order':row['orderId']=loc['id']
            if loc['kind']=='dispenser':row['dispenser']=loc['id']
            history.append(row)
        histories['#'+str(serial)]=history
    expected={id:[{k:v for k,v in h.items() if k!='toBlock'} for h in hs] for id,hs in gold['ids'].items()}
    assert histories==expected, f'{asset}: historical convention mismatch'
    expected_stacks={'address:'+a:[int(s[1:]) for s in ids] for a,ids in gold['owners'].items() if ids}
    for kind,field in [('order','sell_orders'),('dispenser','dispensers')]:
        expected_stacks.update({kind+':'+id:[int(s[1:]) for s in v['ids']] for id,v in gold[field].items() if v['ids']})
    assert {k:v for k,v in stacks.items() if v}==expected_stacks,f'{asset}: ordered stack mismatch'
    fixture={'asset':asset,'scope':'offline legacy convention; not current Core adapter verification','source_sha256':hashlib.sha256(raw_path.read_bytes()).hexdigest(),'golden_sha256':hashlib.sha256(gold_path.read_bytes()).hexdigest(),'operations':ops,'histories':expected,'holdings':expected_stacks}
    (out/f'legacy_{asset}.json').write_text(json.dumps(fixture,separators=(',',':'))+'\n')
    print(asset,len(histories),'serial histories,',len(ops),'operations; complete legacy match')
