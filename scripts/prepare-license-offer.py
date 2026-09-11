"""Validate and freeze a proprietary offer; never modify an existing grant."""
import argparse
import hashlib
import json
from pathlib import Path
import re

ROOT=Path(__file__).resolve().parent.parent

def prepare(source):
    value=json.loads(Path(source).read_text(encoding='utf-8'))
    if value.get('schema_version')!=1 or value.get('status')!='unissued':
        raise ValueError('Expected an unissued schema-1 offer')
    party=value.get('licensor',{})
    for field in ('legal_name','country','service_address','contact_email'):
        if not isinstance(party.get(field),str) or not party[field].strip():
            raise ValueError('Actual licensor '+field+' is required')
    if not re.fullmatch(r'[^@\s]+@[^@\s]+\.[^@\s]+',party['contact_email']):
        raise ValueError('A valid licensor contact email is required')
    if not re.fullmatch(r'(?:[3-9]|[1-9]\d+)\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?',value.get('version','')):
        raise ValueError('Use a new version 3.0.0 or later; old Apache releases cannot be replaced')
    if value.get('license_id')!='LicenseRef-PicoVolt-Proprietary-1.0':
        raise ValueError('Unexpected license identifier')
    if not value.get('covered_components') or value.get('rights_inventory_confirmed') is not True:
        raise ValueError('Explicit proprietary component scope and confirmed rights inventory are required')
    if not re.fullmatch('[a-f0-9]{64}',value.get('artifact_sha256','')):
        raise ValueError('Exact release artifact SHA-256 is required')
    fee=value.get('one_time_fee',{})
    if type(fee.get('amount')) not in (int,float) or not 0<=fee['amount']<1e9 or not fee.get('tax_treatment') or fee.get('currency')!='EUR':
        raise ValueError('A finite non-negative EUR price and tax treatment are required')
    if not value.get('customer_scope') or not value.get('support_included'):
        raise ValueError('Customer and support scope must be stated')
    terms=(ROOT/'legal/PICOVOLT-PROPRIETARY-LICENSE-1.0.md').read_bytes()
    digest=hashlib.sha256(terms).hexdigest()
    if value.get('license_sha256') not in ('',digest):
        raise ValueError('Terms digest mismatch; review the changed terms')
    value['license_sha256']=digest
    value['status']='prepared'
    value['issued_at']=None
    payload=(json.dumps(value,sort_keys=True,indent=2)+'\n').encode()
    output=Path(source).with_name('offer-'+hashlib.sha256(payload).hexdigest()+'.json')
    with output.open('xb') as target: target.write(payload)
    return output

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('offer')
    args=parser.parse_args()
    try: print(prepare(args.offer))
    except (ValueError,OSError,KeyError) as error: parser.exit(1,str(error)+'\n')
