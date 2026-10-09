"""Bounded CI facts for a waiting workspace; no campaign decisions."""
import re
from common import encoded

POLL_SECONDS=15
MAX_READS=128
OUTPUT_BYTES=256*1024


def validate_request(profile,args):
    if not isinstance(args,dict) or set(args)!={'repository','revision'}:
        raise ValueError('wait_for_checks requires repository and revision')
    if not isinstance(args['revision'],str) or not re.fullmatch(r'[0-9a-f]{40}',args['revision']):
        raise ValueError('wait_for_checks requires a complete Git revision')
    policy=next((r for r in profile['verification']['repositories'] if r['repository']==args['repository']),None)
    if policy is None:
        raise ValueError('wait_for_checks repository is outside the fixed profile')
    return list(policy['required_checks'])


def facts(repository,revision,required,page):
    if (not isinstance(page,dict) or type(page.get('total_count')) is not int or
            not 0<=page['total_count']<=100 or not isinstance(page.get('check_runs'),list) or
            len(page['check_runs'])!=page['total_count']):
        raise ValueError('check catalogue is incomplete or exceeds the bounded page')
    selected=[];missing=[]
    for name in required:
        candidates=[]
        for check in page['check_runs']:
            if not isinstance(check,dict):
                raise ValueError('invalid check observation')
            if check.get('name')==name and check.get('head_sha')==revision:
                if (type(check.get('id')) is not int or not isinstance(check.get('status'),str) or
                        check.get('started_at') is not None and not isinstance(check['started_at'],str) or
                        check.get('conclusion') is not None and not isinstance(check['conclusion'],str) or
                        not isinstance(check.get('html_url'),str)):
                    raise ValueError('invalid required check observation')
                candidates.append(check)
        if not candidates:
            missing.append(name);continue
        check=max(candidates,key=lambda c:(c.get('started_at') or '',c['id']))
        selected.append({k:check[k] for k in ('id','name','status','conclusion','html_url')})
    failed=any(c['status']=='completed' and c['conclusion']!='success' for c in selected)
    state='failed' if failed else 'waiting' if missing or any(c['status']!='completed' for c in selected) else 'passed'
    result={'state':state,'repository':repository,'revision':revision,
            'required_checks':required,'checks':selected,'missing':missing}
    if len(encoded(result))>32768:
        raise ValueError('selected check facts exceed reply bound')
    return result
