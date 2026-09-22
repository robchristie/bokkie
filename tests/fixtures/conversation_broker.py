"""Deterministic broker peer for HTTP integration tests; never launches Codex."""
import json
from pathlib import Path
import sys

request = json.loads(sys.stdin.readline())
assert request['profile']['codex'] == '/usr/bin/true'
assert request['profile']['bwrap'] == '/usr/bin/true'
context = request['context']
text = next(message['text'] for message in reversed(context['messages'])
            if message['role'] == 'user')
def output(proposal):
    operation = proposal['operation']
    names = {'save_definition': 'bokkie_save_draft', 'discuss': 'bokkie_discuss',
             'lookup': 'bokkie_lookup', 'preview': 'bokkie_preview', 'propose': 'bokkie_propose'}
    if operation == 'save_definition':
        definition = proposal['definition']
        args = {key: definition[key] for key in ('name', 'purpose', 'instructions', 'capability', 'trigger', 'context_refs') if key in definition}
    else:
        args = {key: value for key, value in proposal.items() if key != 'operation'}
    print(json.dumps({'tool': names[operation], 'arguments': args}))

scenario = request['profile']['model']
assert scenario in ('fixture-ui', 'fixture-empty', 'fixture-matches', 'fixture-fail',
                    'fixture-invalid-json', 'fixture-malformed', 'fixture-read-fail',
                    'fixture-repeat')
if scenario == 'fixture-ui':
    # Exact fixed qualification inputs only. This is a test script, not a natural
    # language implementation or an alternative to live model acceptance.
    import copy

    def note(name, instructions, cron=None):
        return {
            'name': name, 'purpose': name, 'instructions': instructions,
            'context_refs': [],
            'trigger': ({'kind': 'recurring', 'cron': cron, 'timezone': 'Australia/Adelaide'}
                        if cron else {'kind': 'immediate'}),
            'capability': 'local_note', 'profile_revision': 'local-note-v1',
            'effects': ['store_local_result'], 'max_attempts': 3,
            'max_output_chars': 8192, 'destination': 'task_results',
        }

    def save(definition):
        return {'operation': 'save_definition', 'definition': definition,
                'message': 'Synthetic qualification draft; no operation has executed.'}

    def selected_definition():
        selected = context['selected_task']
        return copy.deepcopy((selected.get('candidate') or selected['active'])['definition'])

    if text == "Help me set up a weekday reminder to review my research queue at 9 am Adelaide time. Don't activate it yet.":
        if 'lookup_result' not in context:
            proposal = {'operation': 'lookup', 'query': 'research queue'}
        else:
            assert context['lookup_result']['successful'] is True
            assert context['lookup_result']['items'] == []
            proposal = save(note('Research queue reminder', 'Review my research queue.', '0 9 * * Mon-Fri'))
    elif text == 'Change the reminder text to: Review the research queue and choose one paper to read. Keep it inactive.':
        definition = selected_definition()
        definition['instructions'] = 'Review the research queue and choose one paper to read.'
        proposal = save(definition)
    elif text == 'What exactly will happen? Preview it.':
        proposal = {'operation': 'preview'}
    elif text == 'Find the research queue reminder.':
        proposal = {'operation': 'lookup', 'query': 'research queue'}
    elif text == 'Make it Monday mornings instead, still at 9 am Adelaide time.':
        definition = selected_definition()
        definition['trigger'] = {'kind': 'recurring', 'cron': '0 9 * * Mon', 'timezone': 'Australia/Adelaide'}
        proposal = save(definition)
    elif text == 'Pause this reminder.':
        proposal = {'operation': 'propose', 'action': 'pause'}
    elif text == 'Resume this reminder.':
        proposal = {'operation': 'propose', 'action': 'resume'}
    elif text == 'Create a one-off local note now saying: Remember to organise the reading list.':
        proposal = save(note('Reading list note', 'Remember to organise the reading list.'))
    elif text == 'Save a draft for an AI/ML research finder every weekday at 8 am Adelaide time, to find new papers relevant to my research queue.':
        definition = note('AI/ML research finder', 'Find new papers relevant to my research queue.', '0 8 * * Mon-Fri')
        definition.update(capability='research_finder', profile_revision='research-unavailable-v1')
        proposal = save(definition)
    else:
        raise ValueError('input is outside the closed synthetic UI journey')
    output(proposal)
    sys.exit(0)

control = json.loads(text)
with Path(control['record']).open('a') as stream:
    stream.write(json.dumps(request) + '\n')
if scenario == 'fixture-fail':
    print(json.dumps({'error': 'synthetic broker failure'}))
    sys.exit(1)
if scenario == 'fixture-invalid-json':
    print('synthetic invalid JSON')
    sys.exit(0)
if scenario == 'fixture-malformed':
    proposal = {'operation': 'save_definition', 'definition': {'instructions': 'replace'}}
elif scenario == 'fixture-read-fail':
    proposal = {'operation': 'lookup', 'query': 'x' * 201}
elif scenario == 'fixture-repeat' or 'lookup_result' not in context:
    proposal = {'operation': 'lookup', 'query': 'Adapter needle'}
else:
    assert context['lookup_result']['successful'] is True
    assert context['lookup_result']['items'] == []
    assert all(tool['name'] != 'bokkie_lookup' for tool in request['tools'])
    proposal = {
        'operation': 'save_definition', 'message': 'Prepared the requested local note.',
        'definition': {
            'name': 'Adapter needle', 'purpose': 'A fixture local reminder',
            'instructions': 'Keep this supplied reminder text.', 'context_refs': [],
            'trigger': {'kind': 'immediate'}, 'capability': 'local_note',
            'profile_revision': 'local-note-v1', 'effects': ['store_local_result'],
            'max_attempts': 3, 'max_output_chars': 8192, 'destination': 'task_results',
        },
    }
output(proposal)
