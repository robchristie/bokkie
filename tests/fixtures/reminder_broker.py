"""Closed deterministic reminder journey peer; never starts a model or sends mail."""
import copy
import json
import sys

request = json.loads(sys.stdin.readline())
assert request['profile']['codex'] == '/usr/bin/true'
assert request['profile']['bwrap'] == '/usr/bin/true'
context = request['context']
text = context['current_request']

def tool(name, arguments):
    assert any(t['name'] == name for t in request['tools'])
    print(json.dumps({'tool': name, 'arguments': arguments}))

if text == 'Every weekday at 9, remind me to review today’s priorities.':
    tool('bokkie_discuss', {'message': 'Do you mean 9 am or 9 pm? I’ll use Australia/Adelaide.', 'reason': 'clarification'})
elif text == '9 am, please.':
    tool('bokkie_save_draft', {
        'name': 'Review today’s priorities', 'purpose': 'Start the weekday with clear priorities',
        'instructions': 'Review today’s priorities.', 'capability': 'reminder', 'context_refs': [],
        'trigger': {'kind': 'recurring', 'cron': '0 9 * * Mon-Fri', 'timezone': 'Australia/Adelaide'},
    })
elif text == 'Change it to 10 am on weekdays.':
    selected = context['selected_task']
    definition = copy.deepcopy((selected.get('candidate') or selected['active'])['definition'])
    definition['trigger']['cron'] = '0 10 * * Mon-Fri'
    tool('bokkie_save_draft', {key: definition[key] for key in ('name', 'purpose', 'instructions', 'capability', 'context_refs', 'trigger')})
elif text in ('Pause this reminder.', 'Resume this reminder.', 'Yes, go ahead.'):
    action = {'Pause this reminder.': 'pause', 'Resume this reminder.': 'resume', 'Yes, go ahead.': 'activate'}[text]
    tool('bokkie_propose', {'action': action})
elif text == 'Preview this task':
    tool('bokkie_preview', {})
else:
    raise ValueError('Input is outside the closed synthetic reminder journey')
