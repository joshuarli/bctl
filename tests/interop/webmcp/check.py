import argparse
import hashlib
import http.cookiejar
import json
import os
import shlex
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description='Qualify native WebMCP against a trusted HTTPS fixture and agent-browser CDP.')
    parser.add_argument('--bctl', type=Path, required=True)
    parser.add_argument('--home', type=Path, required=True)
    parser.add_argument('--session', required=True)
    parser.add_argument('--url', required=True)
    parser.add_argument('--image', type=Path, required=True)
    parser.add_argument('--agent-browser', default='agent-browser')
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    env = {**os.environ, 'BCTL_HOME': str(args.home.resolve())}
    checks = []

    def command(argv, error=None):
        result = subprocess.run([str(arg) for arg in argv], env=env, capture_output=True, text=True, timeout=90)
        if error:
            assert result.returncode != 0 and error in result.stderr, (error, result.returncode, result.stderr)
            return None
        if result.returncode:
            raise RuntimeError(f'{Path(argv[0]).name} failed: {result.stderr}')
        return json.loads(result.stdout)

    def bctl(*argv, error=None):
        return command([args.bctl, *argv], error)

    def evaluate(expression):
        response = bctl('page', 'eval', args.session, expression, '--json')
        assert response['error'] is None, response['error']
        return response['page']['action']['value']

    def catalog():
        return bctl('webmcp', 'list', args.session)

    def invoke(name, payload=None, *flags, error=None):
        return bctl('webmcp', 'call', args.session, name, json.dumps(payload or {}), *flags, error=error)

    def wait_for(expression):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            value = evaluate(expression)
            if value:
                return value
            time.sleep(.05)
        raise RuntimeError(f'fixture condition timed out: {expression}')

    def refuse(name, error, *flags):
        before = evaluate('fixtureCalls')
        invoke(name, {}, *flags, error=error)
        assert evaluate('fixtureCalls') == before
        checks.append(f'refusal:{name}:{error}:{",".join(flags)}')

    bctl('page', 'goto', args.session, args.url, '--json')
    wait_for('fixtureReady || fixtureError')
    assert evaluate('fixtureError') is None
    assert evaluate('isSecureContext') is True
    wait_for('foreignReady')
    metadata = evaluate('({ua:navigator.userAgent,origin:location.origin,methods:Object.getOwnPropertyNames(Object.getPrototypeOf(document.modelContext))})')
    assert {'getTools', 'executeTool', 'registerTool'} <= set(metadata['methods'])
    tools = catalog()
    expected = {'snapshot', 'query', 'act', 'submit'}
    assert {tool['name'] for tool in tools} == expected
    assert all(tool['origin'] == metadata['origin'] for tool in tools)
    checks.extend(['secure-context', 'javascript-methods', 'registered-catalog', 'cross-origin-isolation'])
    assert invoke('snapshot')['result'] == {'text': 'idle'}
    hostile = '"; globalThis.__injected = true; // <script>bad</script>'
    assert invoke('query', {'text': hostile})['result'] == {'text': hostile}
    prototype_keys = {'text': 'data', '__proto__': {'injected': True}, 'constructor': {'prototype': {'marker': True}}}
    assert invoke('query', prototype_keys)['result'] == prototype_keys
    checks.append('prototype-keys-are-json-data')
    refuse(hostile, 'WEBMCP_TOOL_MISSING_OR_AMBIGUOUS')
    assert evaluate('typeof globalThis.__injected') == 'undefined'
    checks.extend(['snapshot-callback', 'hostile-json-is-data', 'hostile-name-is-data'])
    refuse('act', 'WEBMCP_WRITE_OPT_IN_REQUIRED')
    refuse('submit', 'WEBMCP_WRITE_OPT_IN_REQUIRED')
    refuse('submit', 'WEBMCP_CONSEQUENTIAL_OPT_IN_REQUIRED', '--allow-write')
    refuse('submit', 'WEBMCP_WRITE_OPT_IN_REQUIRED', '--allow-consequential')
    assert invoke('act', {'text': 'authorized'}, '--allow-write')['result'] == {'text': 'authorized'}
    assert invoke('submit', {}, '--allow-write', '--allow-consequential')['result'] == {'submitted': True}
    assert evaluate('fixtureCalls.act') == 1 and evaluate('fixtureCalls.submit') == 1
    checks.append('distinct-opt-ins-invoke-once')

    capability = bctl('cdp-url', args.session, '--json')['url']
    selected = bctl('tab', 'list', args.session, '--json')['tabs']['selected']
    ab_session = f'bctl-webmcp-{os.getpid()}'
    ab = [args.agent_browser, '--session', ab_session]
    discovery_polls = 0
    try:
        command([*ab, 'connect', capability, '--json'])
        command([*ab, 'tab', selected, '--json'])
        deadline = time.monotonic() + 10
        while True:
            external = command([*ab, 'webmcp', 'list', '--json'])
            discovery_polls += 1
            assert external['success'], external.get('error')
            external_tools = external['data']['tools']
            names = {tool['name'] for tool in external_tools if tool['origin'] == metadata['origin']}
            assert names <= expected, external_tools
            if names == expected:
                break
            assert time.monotonic() < deadline, external_tools
            time.sleep(.05)
        oracle = command([*ab, 'webmcp', 'invoke', 'query', '--params', '{"text":"oracle"}', '--json'])
        assert oracle['success'], oracle.get('error')
        assert oracle['data']['output'] == {'text': 'oracle'}, oracle['data']
        checks.append('agent-browser-cdp-catalog-and-invocation')
    finally:
        had_error = sys.exc_info()[0] is not None
        cleanup_errors = []
        for cleanup in ([args.bctl, 'cdp-revoke', args.session, '--url', capability, '--json'], [*ab, 'close', '--json']):
            try:
                command(cleanup)
            except Exception as error:
                cleanup_errors.append(str(error))
        if cleanup_errors:
            if had_error:
                print('CDP cleanup failed: ' + '; '.join(cleanup_errors), file=sys.stderr)
            else:
                raise RuntimeError('CDP cleanup failed: ' + '; '.join(cleanup_errors))

    evaluate('registerFixtureTool("ephemeral", {readOnlyHint:true}, () => ({registered:true})).then(() => true)')
    assert 'ephemeral' in {tool['name'] for tool in catalog()}
    assert invoke('ephemeral')['result'] == {'registered': True}
    checks.append('dynamic-registration')
    before = evaluate('fixtureCalls')
    duplicate = evaluate('(async () => { try { await registerFixtureTool("query", {readOnlyHint:true}, () => ({duplicate:true})); return null; } catch (error) { return error.name; } })()')
    assert duplicate == 'InvalidStateError'
    assert sum(tool['name'] == 'query' for tool in catalog()) == 1
    assert evaluate('fixtureCalls') == before
    assert invoke('query', {'text': 'original'})['result'] == {'text': 'original'}
    checks.append('duplicate-registration-preserves-original')
    bctl('page', 'goto', args.session, args.url, '--json')
    wait_for('fixtureReady || fixtureError')
    assert evaluate('fixtureError') is None
    assert {tool['name'] for tool in catalog()} == expected
    assert evaluate('fixtureCalls') == {'snapshot': 0, 'query': 0, 'act': 0, 'submit': 0}
    refuse('ephemeral', 'WEBMCP_TOOL_MISSING_OR_AMBIGUOUS')
    checks.append('navigation-clears-registration')

    login = bctl('serve', '--json')['url']
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    with opener.open(login, timeout=10) as response:
        response.read()
    parts = urllib.parse.urlsplit(login)
    origin = f'{parts.scheme}://{parts.netloc}'
    control_url = f'{origin}/api/sessions/{urllib.parse.quote(args.session, safe="")}/control'

    def control(suffix=''):
        request = urllib.request.Request(control_url + suffix, data=b'', headers={'Origin': origin}, method='POST')
        with opener.open(request, timeout=10) as response:
            return json.load(response)

    before = evaluate('fixtureCalls')
    session = bctl('inspect', args.session, '--json')['sessions'][0]
    if session['input'] == 'headless':
        try:
            control()
        except urllib.error.HTTPError as error:
            assert error.code == 409 and b'no input devices' in error.read()
        else:
            raise AssertionError('headless runtime accepted human input control')
        checks.append('headless-takeover-is-unsupported')
    else:
        try:
            assert control()['authority'] == 'human'
            bctl('webmcp', 'list', args.session, error='HUMAN_IN_CONTROL')
            invoke('act', {'text': 'refused'}, '--allow-write', error='HUMAN_IN_CONTROL')
        finally:
            assert control('/resume')['authority'] == 'automation'
        checks.append('human-takeover-never-invokes')
    assert evaluate('fixtureCalls') == before
    bctl('page', 'goto', args.session, 'chrome://version', '--json')
    command_line = evaluate('document.querySelector("#command_line").textContent')
    feature_flags = [arg for arg in shlex.split(command_line) if arg.startswith('--enable-features=')]
    assert feature_flags
    for feature in ('WebMCPTesting', 'DevToolsWebMCPSupport'):
        assert feature in feature_flags[-1].split('=', 1)[1].split(',')
    checks.append('effective-feature-flags')
    image_digest = hashlib.sha256()
    with args.image.open('rb') as image:
        for chunk in iter(lambda: image.read(1024 * 1024), b''):
            image_digest.update(chunk)
    receipt = {
        'status': 'PASS', 'checks': checks, 'browser': metadata,
        'browser_command_line': command_line,
        'display': session['display'],
        'image_sha256': image_digest.hexdigest(),
        'cdp_catalog_polls': discovery_polls,
        'features': ['WebMCPTesting', 'DevToolsWebMCPSupport'],
        'certificate_verification': 'fixture CA trusted in test-image NSS database',
        'agent_browser_version': subprocess.check_output([args.agent_browser, '--version'], text=True).strip(),
    }
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
    print(f'PASS WebMCP: {len(checks)} checks; receipt {args.receipt}')


if __name__ == '__main__':
    main()
