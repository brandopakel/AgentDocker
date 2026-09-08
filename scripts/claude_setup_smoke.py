#!/usr/bin/env python3
"""Exercise packaged saved setup against the real Claude configuration CLI.

Private profile directories only; no model, credentials or daemon are required.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None


def run(binary, manifest_path, output):
    output.mkdir(mode=0o700)
    manifest = json.loads(manifest_path.read_text())
    assert digest(binary) == manifest['binary_sha256']['agentdocker']
    report = {
        'scope': 'Actual packaged preview/apply/health/undo with installed Claude MCP configuration CLI; no model session',
        'source_commit': manifest['source_commit'],
        'source_tree': manifest['source_tree'],
        'binary_sha256': digest(binary),
        'driver_sha256': digest(Path(__file__)),
        'checks': [], 'calls': [], 'result': 'failed',
    }
    user_files = [Path.home()/'.claude.json', Path.home()/'.claude/settings.json']
    original = [digest(p) for p in user_files]
    started = time.monotonic()
    scratch = None
    try:
        with tempfile.TemporaryDirectory(prefix='ad-claude-setup-', dir='/tmp') as directory:
            scratch = Path(directory).resolve()
            profile_a, profile_b = scratch/'profile-a', scratch/'profile-b'
            profile_a.mkdir(); profile_b.mkdir()
            state = scratch/'state'
            for profile in [profile_a, profile_b]:
                (profile/'.claude.json').write_text(json.dumps({'mcpServers': {
                    'unrelated-fixture': {'type':'stdio','command':'/usr/bin/true','args':[],'env':{}}
                }})+'\n')
            original_b = (profile_b/'.claude.json').read_bytes()
            hooks = profile_a/'settings.json'
            original_hooks = json.dumps({'hooks': {'UserPromptSubmit': [
                {'hooks': [{'type':'command', 'command':'/usr/bin/true'}]}
            ]}})+'\n'
            hooks.write_text(original_hooks)
            env = {k:v for k,v in os.environ.items() if not k.startswith(('AGENTDOCKER_', 'CLAUDE', 'ANTHROPIC'))}
            env.update(AGENTDOCKER_HOME=str(state), AGENTDOCKER_SOCKET=str(scratch/'absent.sock'),
                       AGENTDOCKER_NO_AUTOSTART='1', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1')

            def call(name, profile, args, success=True):
                invocation_env = dict(env, CLAUDE_CONFIG_DIR=str(profile))
                begin = time.monotonic()
                completed = subprocess.run([str(binary), 'setup', *args, '--json'],
                                           cwd=scratch, env=invocation_env, capture_output=True,
                                           text=True, timeout=45)
                (output/f'{len(report["calls"]):02}-{name}.stdout').write_text(completed.stdout)
                (output/f'{len(report["calls"]):02}-{name}.stderr').write_text(completed.stderr)
                report['calls'].append({'name':name, 'exit':completed.returncode,
                                        'elapsed_seconds':time.monotonic()-begin})
                assert (completed.returncode == 0) == success, f'{name}: unexpected exit {completed.returncode}'
                return json.loads(completed.stdout) if success else None

            def preview(name):
                plan = call(name, profile_a, ['claude-code', '--preview'])
                private = json.loads((state/'setup'/f'{plan["id"]}.json').read_text())
                assert len(private['delegated']) == 1, 'preview did not plan selected-profile MCP registration'
                assert Path(private['delegated'][0]['config_dir']) == profile_a
                assert Path(private['delegated'][0]['path']) == profile_a/'.claude.json'
                assert all(Path(c['path']).is_relative_to(profile_a) for c in private['changes'])
                return plan, private

            def provider_state():
                return json.loads((profile_a/'.claude.json').read_text())

            def assert_other_profile():
                assert (profile_b/'.claude.json').read_bytes() == original_b, 'saved action touched invoking profile B'

            first, private = preview('preview-a')
            version_env = dict(env, CLAUDE_CONFIG_DIR=str(profile_a))
            report['provider_version'] = subprocess.check_output(
                [private['delegated'][0]['add'][0], '--version'], cwd=scratch,
                env=version_env, text=True, timeout=5).strip()
            report['checks'].append('preview selects profile A for hooks and MCP')
            call('apply-from-b', profile_b, ['--apply', first['id']])
            expected = private['delegated'][0]['expected']
            assert provider_state()['mcpServers']['agentdocker'] == expected
            assert 'unrelated-fixture' in provider_state()['mcpServers']
            assert_other_profile()
            report['checks'].append('actual provider add uses saved profile A while caller selects B; exact entry and unrelated MCP preserved')
            health = call('health-a', profile_a, ['claude-code', '--health'])
            runtime = health['runtimes'][0]
            assert runtime['name'] == 'claude-code'
            assert runtime['mcp_configuration'] == 'wired' and runtime['hooks_configuration'] == 'wired'
            assert all(check['status'] == 'executable_available' for check in runtime['checks'])
            assert not health['daemon_reachable'], 'fixture must not connect to production daemon'
            report['checks'].append('health agrees on selected-profile MCP and hooks; isolated absent daemon remains disconnected')
            call('undo-from-b', profile_b, ['--undo', first['id']])
            assert 'agentdocker' not in provider_state()['mcpServers']
            assert 'unrelated-fixture' in provider_state()['mcpServers']
            assert hooks.read_text() == original_hooks
            assert_other_profile()
            report['checks'].append('actual provider removal uses saved profile A, preserves unrelated MCP and restores original hooks')

            second, second_private = preview('preview-changed-entry')
            call('apply-changed-entry', profile_b, ['--apply', second['id']])
            changed = provider_state()
            changed['mcpServers']['agentdocker']['env']['AGENTDOCKER_SOCKET'] = str(scratch/'another.sock')
            changed_raw = json.dumps(changed)+'\n'
            (profile_a/'.claude.json').write_text(changed_raw)
            installed_hooks = hooks.read_bytes()
            call('refuse-changed-entry', profile_b, ['--undo', second['id']], success=False)
            assert (profile_a/'.claude.json').read_text() == changed_raw
            assert hooks.read_bytes() == installed_hooks
            assert_other_profile()
            report['checks'].append('changed environment refuses undo before hooks or provider entry are changed')
            changed['mcpServers']['agentdocker'] = second_private['delegated'][0]['expected']
            (profile_a/'.claude.json').write_text(json.dumps(changed)+'\n')
            call('undo-restored-entry', profile_b, ['--undo', second['id']])
            assert hooks.read_text() == original_hooks

            third, _ = preview('preview-malformed-state')
            valid = (profile_a/'.claude.json').read_text()
            (profile_a/'.claude.json').write_text('{')
            call('refuse-malformed-state', profile_b, ['--apply', third['id']], success=False)
            assert (profile_a/'.claude.json').read_text() == '{'
            assert hooks.read_text() == original_hooks
            assert_other_profile()
            report['checks'].append('malformed provider state refuses apply before hook edits')
            (profile_a/'.claude.json').write_text(valid)
            call('close-unused-plan', profile_b, ['--undo', third['id']])
            assert not (scratch/'absent.sock').exists()
            report['result'] = 'passed'
    except Exception as error:
        report['error'] = str(error)
    finally:
        report['elapsed_seconds'] = time.monotonic()-started
        report['cleanup'] = {
            'scratch_removed': scratch is None or not scratch.exists(),
            'user_configuration_hashes_unchanged': original == [digest(p) for p in user_files],
            'model_invoked': False,
        }
        if not all(report['cleanup'][key] for key in ['scratch_removed','user_configuration_hashes_unchanged']):
            report['result'] = 'failed'
        (output/'result.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))
    return 0 if report['result']=='passed' else 1


if __name__ == '__main__':
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--manifest',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args = parser.parse_args()
    raise SystemExit(run(args.binary.resolve(strict=True),args.manifest.resolve(strict=True),args.output))
