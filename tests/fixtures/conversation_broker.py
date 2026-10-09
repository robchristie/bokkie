"""Deterministic broker peer for HTTP integration tests; never launches Codex."""
import json
from pathlib import Path
import sys

request = json.loads(sys.stdin.readline())
assert request['profile']['codex'] == '/usr/bin/true'
assert request['profile']['bwrap'] == '/usr/bin/true'
SCENARIOS = ('fixture-workspace', 'fixture-handoff', 'fixture-ui', 'fixture-settings', 'fixture-empty', 'fixture-matches', 'fixture-fail',
             'fixture-invalid-json', 'fixture-malformed', 'fixture-read-fail',
             'fixture-repeat', 'fixture-adviser-main', 'fixture-adviser',
             'fixture-adviser-fail', 'fixture-adviser-malformed', 'fixture-adviser-timeout',
             'fixture-adviser-hang')
if request.get('models'):
    print(json.dumps({'codex_version':'0.160.0','models':[{
        'model':model,'displayName':model,'defaultReasoningEffort':'medium',
        'supportedReasoningEfforts':[{'reasoningEffort':effort,'description':effort} for effort in ('low','medium','high')]
    } for model in SCENARIOS], 'model_calls':0}))
    sys.exit(0)
context = request['context']
text = context['current_request']
def output(proposal):
    operation = proposal['operation']
    names = {'save_definition': 'bokkie_save_draft', 'discuss': 'bokkie_discuss',
             'lookup': 'bokkie_lookup', 'preview': 'bokkie_preview', 'propose': 'bokkie_propose',
             'prepare_handoff': 'bokkie_prepare_handoff', 'prepare_workspace': 'bokkie_workspace_task'}
    if operation == 'save_definition':
        definition = proposal['definition']
        args = {key: definition[key] for key in ('name', 'purpose', 'instructions', 'capability', 'trigger', 'context_refs') if key in definition}
    else:
        args = {key: value for key, value in proposal.items() if key != 'operation'}
    print(json.dumps({'tool': names[operation], 'arguments': args}))

scenario = request['profile']['model']
assert scenario in SCENARIOS
if scenario == 'fixture-workspace':
    output({'operation':'prepare_workspace','project_query':'Atlas','brief':{
        'outcome':'Keep the documentation aligned with the code',
        'context':'Retain existing examples and verified results.',
        'constraints':'Original scope: ordinary source corrections only; no deployment.',
        'acceptance':'Deliver reviewed documentation corrections with required checks and attributable revisions.',
        'references':[]}})
    sys.exit(0)
if scenario == 'fixture-handoff':
    if text == 'Prepare a hand-off for Atlas to add a searchable project list with Australian English labels.':
        output({'operation': 'prepare_handoff', 'project_query': 'Atlas', 'brief': {
            'outcome': 'Add a searchable project list with Australian English labels.',
            'context': 'Use the existing project navigation and retain the current selection.',
            'constraints': 'Keep the change within the selected project workspace. Do not deploy or publish.',
            'acceptance': 'Search filters the project list. Clearing search restores every project. Labels use Australian English. Existing navigation still works.',
            'references': []}})
    elif text == 'Let’s discuss a searchable project list for Atlas. Keep the current selection when searching.':
        output({'operation':'discuss','reason':'exploration','message':'We can retain the current project selection while filtering the list. Ask for a hand-off when ready.'})
    else:
        raise ValueError('input is outside the closed synthetic hand-off journey')
    sys.exit(0)
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
def barrier(path):
    if not path:
        return
    import time
    deadline = time.monotonic() + 4
    while not Path(path).exists():
        if time.monotonic() >= deadline:
            raise ValueError('synthetic barrier expired')
        time.sleep(0.001)

if request.get('output_schema'):
    assert scenario.startswith('fixture-adviser') and scenario != 'fixture-adviser-main'
    assert 'tools' not in request
    assert set(context) == {'additional_instructions', 'current_request', 'question',
                            'conflicting_requirements', 'selected_task_summary'}
    assert request['output_schema'] == {'type': 'object', 'properties': {'advice': {
        'type': 'string', 'minLength': 1}}, 'required': ['advice'], 'additionalProperties': False}
    assert 'no tools' in request['instructions']
    barrier(control.get('adviser_release'))
    if scenario == 'fixture-adviser-fail':
        print(json.dumps({'error': 'synthetic Astra failure'}))
        sys.exit(1)
    if scenario == 'fixture-adviser-timeout':
        print(json.dumps({'error': 'conversation deadline exceeded'}))
        sys.exit(1)
    if scenario == 'fixture-adviser-hang':
        import os
        import signal
        Path(control['adviser_pid']).write_text(str(os.getpid()))
        # No reply and no self-imposed timeout: the Rust supervisor owns stopping
        # and reaping this peer when the saved deadline plus teardown expires.
        while True:
            signal.pause()
    if scenario == 'fixture-adviser-malformed':
        print(json.dumps({'advice': 'forged', 'operation': 'activate'}))
        sys.exit(0)
    print(json.dumps({'advice': 'The requirements conflict. Ask which requirement takes priority; do not invent a task or an approval.'}))
    sys.exit(0)

if 'adviser_result' in context:
    barrier(control.get('return_release'))
elif 'lookup_result' not in context:
    barrier(control.get('release'))

if scenario == 'fixture-adviser-main':
    route = control.get('adviser_route', 'manual')
    if route == 'lookup_first' and 'lookup_result' not in context:
        output({'operation': 'lookup', 'query': 'Adapter needle'})
    elif route in ('lookup_first', 'advice_first', 'invalid_quotes', 'unsupported_condition', 'repeat_advice') and ('adviser_result' not in context or route == 'repeat_advice'):
        quotes = control.get('requirements', ['Only at 9 am', 'Only at 10 am'])
        if route == 'invalid_quotes':
            quotes[1] = 'Invented requirement outside request'
        output({'operation': 'discuss', 'reason': 'difficulty',
                'message': 'I cannot reconcile these explicit requirements.',
                'difficulty': {'condition': 'unsupported' if route == 'unsupported_condition' else 'conflicting_requirements',
                               'question': 'Which requirement should Bokkie prioritise?',
                               'requirements': quotes}})
    elif route == 'advice_first' and 'lookup_result' not in context:
        output({'operation': 'lookup', 'query': 'Adapter needle'})
    else:
        result = context.get('adviser_result')
        if result is None:
            message = 'Bokkie answered without a consultation.'
        elif result['status'] == 'completed':
            message = 'Bokkie considered Astra’s advice. Which requirement takes priority?'
        else:
            message = 'Bokkie continues: Astra consultation failed. ' + result['error']
        output({'operation': 'discuss', 'reason': 'answer', 'message': message})
    sys.exit(0)
if scenario == 'fixture-settings':
    output({'operation':'discuss','reason':'answer','message':json.dumps({'model':scenario,'effort':request['profile']['effort'],'instructions':context['additional_instructions'],'timeout':request['profile']['timeout_seconds']})})
    sys.exit(0)
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
